// SPDX-License-Identifier: Apache-2.0
pragma solidity ^0.8.0;
/// A token.
contract Token {
    struct Holder { address who; }
    event Transfer(address from);
    modifier onlyOwner() { _; }
    function transfer(address to, uint256 v) public onlyOwner returns (bool) { emit Transfer(to); return check(v); }
    function check(uint256 v) internal pure returns (bool) { return v > 0; }
}
interface IToken { function transfer(address to, uint256 v) external returns (bool); }
