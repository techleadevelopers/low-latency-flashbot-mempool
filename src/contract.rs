// src/contract.rs
use ethers::prelude::abigen;

abigen!(
    Simple7702Delegate,
    r#"[
        function sweepNative() external
        function sweepTokens(address[] calldata tokens) external
        function sweepAll(address[] calldata tokens) external
        function sweepArbitrum() external
        function sweepBSC() external
        function setDestination(address _dest) external
        function setFrozen(bool _frozen) external
        function emergencyWithdraw() external
        function emergencyWithdrawToken(address token) external
        function addArbitrumToken(address token) external
        function addBscToken(address token) external
        function getArbitrumTokens() external view returns (address[] memory)
        function getBscTokens() external view returns (address[] memory)
        function getNativeBalance() external view returns (uint256)
        function getTokenBalance(address token) external view returns (uint256)
        function destination() external view returns (address)
        function frozen() external view returns (bool)
        function OWNER() external view returns (address)
    ]"#
);

abigen!(
    ERC20Token,
    r#"[
        function balanceOf(address owner) external view returns (uint256)
    ]"#
);
