// SPDX-License-Identifier: MIT
pragma solidity ^0.8.20;

interface IERC20Minimal {
    function balanceOf(address account) external view returns (uint256);
    function transfer(address to, uint256 amount) external returns (bool);
    function transferFrom(address from, address to, uint256 amount) external returns (bool);
    function approve(address spender, uint256 amount) external returns (bool);
}

interface IUniswapV2Pair {
    function token0() external view returns (address);
    function token1() external view returns (address);
    function swap(uint256 amount0Out, uint256 amount1Out, address to, bytes calldata data) external;
}

interface IUniswapV2Router {
    function swapExactTokensForTokens(
        uint256 amountIn,
        uint256 amountOutMin,
        address[] calldata path,
        address to,
        uint256 deadline
    ) external returns (uint256[] memory amounts);
}

library SafeERC20Minimal {
    function safeTransfer(address token, address to, uint256 amount) internal {
        (bool ok, bytes memory data) = token.call(
            abi.encodeWithSelector(IERC20Minimal.transfer.selector, to, amount)
        );
        require(ok && (data.length == 0 || abi.decode(data, (bool))), "TRANSFER_FAILED");
    }

    function safeTransferFrom(address token, address from, address to, uint256 amount) internal {
        (bool ok, bytes memory data) = token.call(
            abi.encodeWithSelector(IERC20Minimal.transferFrom.selector, from, to, amount)
        );
        require(ok && (data.length == 0 || abi.decode(data, (bool))), "TRANSFER_FROM_FAILED");
    }

    function forceApprove(address token, address spender, uint256 amount) internal {
        (bool ok0, ) = token.call(
            abi.encodeWithSelector(IERC20Minimal.approve.selector, spender, 0)
        );
        require(ok0, "APPROVE_RESET_FAILED");
        (bool ok1, bytes memory data) = token.call(
            abi.encodeWithSelector(IERC20Minimal.approve.selector, spender, amount)
        );
        require(ok1 && (data.length == 0 || abi.decode(data, (bool))), "APPROVE_FAILED");
    }
}

contract MevExecutor {
    using SafeERC20Minimal for address;

    struct SwapStep {
        address router;
        address[] path;
        uint256 amountIn;
        uint256 minOut;
    }

    struct FlashParams {
        address pair;
        address borrowToken;
        uint256 borrowAmount;
        uint256 minProfit;
        address profitToken;
        SwapStep[] steps;
    }

    address public immutable owner;
    address public immutable profitRecipient;
    bool private locked;
    address private activePair;

    event Profit(address indexed token, uint256 amount);
    event Execution(uint256 steps, uint256 finalBalance, uint256 repayAmount, uint256 minProfit);

    modifier onlyOwner() {
        require(msg.sender == owner, "NOT_OWNER");
        _;
    }

    modifier nonReentrant() {
        require(!locked, "REENTRANCY");
        locked = true;
        _;
        locked = false;
    }

    constructor(address initialOwner, address initialProfitRecipient) {
        require(initialOwner != address(0), "INVALID_OWNER");
        require(initialProfitRecipient != address(0), "INVALID_RECIPIENT");
        owner = initialOwner;
        profitRecipient = initialProfitRecipient;
    }

    function executeWithCapital(
        address inputToken,
        uint256 amountIn,
        uint256 minProfit,
        SwapStep[] calldata steps
    ) external onlyOwner nonReentrant {
        require(steps.length > 0, "NO_STEPS");
        inputToken.safeTransferFrom(msg.sender, address(this), amountIn);

        uint256 initialBalance = IERC20Minimal(inputToken).balanceOf(address(this));
        _executeSteps(steps);
        uint256 finalBalance = IERC20Minimal(inputToken).balanceOf(address(this));

        require(finalBalance > initialBalance + minProfit, "NO_PROFIT");
        uint256 profit = finalBalance - initialBalance;
        inputToken.safeTransfer(profitRecipient, finalBalance);
        emit Profit(inputToken, profit);
    }

    function startV2FlashSwap(
        address pair,
        address borrowToken,
        uint256 borrowAmount,
        uint256 minProfit,
        address profitToken,
        SwapStep[] calldata steps
    ) external onlyOwner nonReentrant {
        require(pair != address(0), "INVALID_PAIR");
        require(borrowAmount > 0, "ZERO_BORROW");
        require(steps.length > 0, "NO_STEPS");
        address token0 = IUniswapV2Pair(pair).token0();
        address token1 = IUniswapV2Pair(pair).token1();
        require(borrowToken == token0 || borrowToken == token1, "BORROW_NOT_IN_PAIR");

        uint256 amount0Out = borrowToken == token0 ? borrowAmount : 0;
        uint256 amount1Out = borrowToken == token1 ? borrowAmount : 0;
        bytes memory data = abi.encode(
            FlashParams(pair, borrowToken, borrowAmount, minProfit, profitToken, steps)
        );
        activePair = pair;
        IUniswapV2Pair(pair).swap(amount0Out, amount1Out, address(this), data);
        activePair = address(0);
    }

    function uniswapV2Call(address, uint256 amount0, uint256 amount1, bytes calldata data) external {
        FlashParams memory params = abi.decode(data, (FlashParams));
        require(msg.sender == params.pair, "INVALID_CALLBACK");
        require(msg.sender == activePair, "NO_ACTIVE_FLASH");
        uint256 borrowed = amount0 > 0 ? amount0 : amount1;
        require(borrowed == params.borrowAmount, "BORROW_MISMATCH");

        uint256 initialBalance = IERC20Minimal(params.profitToken).balanceOf(address(this));
        _executeSteps(params.steps);

        uint256 repayAmount = _v2RepayAmount(params.borrowAmount);
        params.borrowToken.safeTransfer(params.pair, repayAmount);

        uint256 finalBalance = IERC20Minimal(params.profitToken).balanceOf(address(this));
        require(finalBalance > initialBalance + params.minProfit, "NO_PROFIT");

        uint256 profit = finalBalance - initialBalance;
        params.profitToken.safeTransfer(profitRecipient, profit);
        emit Execution(params.steps.length, finalBalance, repayAmount, params.minProfit);
        emit Profit(params.profitToken, profit);
    }

    function rescueToken(address token, uint256 amount) external onlyOwner nonReentrant {
        token.safeTransfer(profitRecipient, amount);
    }

    function _executeSteps(SwapStep[] memory steps) internal {
        for (uint256 i = 0; i < steps.length; i++) {
            require(steps[i].router != address(0), "INVALID_ROUTER");
            require(steps[i].path.length >= 2, "INVALID_PATH");
            address input = steps[i].path[0];
            uint256 amountIn = steps[i].amountIn;
            if (amountIn == type(uint256).max) {
                amountIn = IERC20Minimal(input).balanceOf(address(this));
            }
            require(amountIn > 0, "ZERO_AMOUNT_IN");

            input.forceApprove(steps[i].router, amountIn);
            IUniswapV2Router(steps[i].router).swapExactTokensForTokens(
                amountIn,
                steps[i].minOut,
                steps[i].path,
                address(this),
                block.timestamp
            );
            input.forceApprove(steps[i].router, 0);
        }
    }

    function _v2RepayAmount(uint256 borrowed) internal pure returns (uint256) {
        return ((borrowed * 1000) / 997) + 1;
    }
}
