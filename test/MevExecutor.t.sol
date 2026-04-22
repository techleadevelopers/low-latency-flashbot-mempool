// SPDX-License-Identifier: MIT
pragma solidity ^0.8.20;

import "../contracts/MevExecutor.sol";

contract MockToken {
    string public name;
    mapping(address => uint256) public balanceOf;
    mapping(address => mapping(address => uint256)) public allowance;

    constructor(string memory n) {
        name = n;
    }

    function mint(address to, uint256 amount) external {
        balanceOf[to] += amount;
    }

    function approve(address spender, uint256 amount) external returns (bool) {
        allowance[msg.sender][spender] = amount;
        return true;
    }

    function transfer(address to, uint256 amount) external returns (bool) {
        require(balanceOf[msg.sender] >= amount, "BAL");
        balanceOf[msg.sender] -= amount;
        balanceOf[to] += amount;
        return true;
    }

    function transferFrom(address from, address to, uint256 amount) external returns (bool) {
        require(balanceOf[from] >= amount, "BAL");
        require(allowance[from][msg.sender] >= amount, "ALLOW");
        allowance[from][msg.sender] -= amount;
        balanceOf[from] -= amount;
        balanceOf[to] += amount;
        return true;
    }
}

contract MockRouter {
    function swapExactTokensForTokens(
        uint256 amountIn,
        uint256 amountOutMin,
        address[] calldata path,
        address to,
        uint256
    ) external returns (uint256[] memory amounts) {
        MockToken(path[0]).transferFrom(msg.sender, address(this), amountIn);
        uint256 amountOut = amountIn * 2;
        require(amountOut >= amountOutMin, "SLIPPAGE");
        MockToken(path[path.length - 1]).mint(to, amountOut);
        amounts = new uint256[](path.length);
        amounts[0] = amountIn;
        amounts[path.length - 1] = amountOut;
    }
}

contract MevExecutorTest {
    function testExecuteWithCapitalEnforcesProfit() external {
        MockToken a = new MockToken("A");
        MockToken b = new MockToken("B");
        MockRouter router = new MockRouter();
        MevExecutor executor = new MevExecutor(address(this), address(this));

        a.mint(address(this), 100 ether);
        a.approve(address(executor), 100 ether);

        MevExecutor.SwapStep[] memory steps = new MevExecutor.SwapStep[](2);
        address[] memory path = new address[](2);
        address[] memory pathBack = new address[](2);
        path[0] = address(a);
        path[1] = address(b);
        pathBack[0] = address(b);
        pathBack[1] = address(a);
        steps[0] = MevExecutor.SwapStep({
            router: address(router),
            path: path,
            amountIn: 10 ether,
            minOut: 19 ether
        });
        steps[1] = MevExecutor.SwapStep({
            router: address(router),
            path: pathBack,
            amountIn: type(uint256).max,
            minOut: 39 ether
        });

        executor.executeWithCapital(address(a), 10 ether, 1 ether, steps);
        require(a.balanceOf(address(this)) == 130 ether, "PROFIT_NOT_SENT");
    }
}
