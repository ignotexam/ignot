// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

import {Test} from "forge-std/Test.sol";
import {IERC20Like, StumpBox} from "../src/StumpBox.sol";

contract MockToken {
    mapping(address => uint256) public balanceOf;
    mapping(address => mapping(address => uint256)) public allowance;

    function mint(address to, uint256 amount) external {
        balanceOf[to] += amount;
    }

    function approve(address spender, uint256 amount) external returns (bool) {
        allowance[msg.sender][spender] = amount;
        return true;
    }

    function transferFrom(address from, address to, uint256 amount) external virtual returns (bool) {
        require(allowance[from][msg.sender] >= amount, "allowance");
        require(balanceOf[from] >= amount, "balance");
        allowance[from][msg.sender] -= amount;
        balanceOf[from] -= amount;
        balanceOf[to] += amount;
        return true;
    }
}

/// Old-style token whose transferFrom returns nothing.
contract SilentToken is MockToken {
    function transferFrom(address from, address to, uint256 amount) external override returns (bool) {
        require(allowance[from][msg.sender] >= amount && balanceOf[from] >= amount, "no");
        allowance[from][msg.sender] -= amount;
        balanceOf[from] -= amount;
        balanceOf[to] += amount;
        assembly {
            return(0, 0)
        }
    }
}

/// Token that reports failure instead of reverting.
contract FalseToken is MockToken {
    function transferFrom(address, address, uint256) external pure override returns (bool) {
        return false;
    }
}

contract StumpBoxTest is Test {
    MockToken internal token;
    StumpBox internal box;
    address internal sealer = address(0x5EA1);
    address internal alice = address(0xA11CE);
    bytes32 internal constant ID = keccak256("exam-1");
    uint256 internal constant BURN_AMOUNT = 100_000 ether;
    bytes internal ct = bytes("-----BEGIN AGE ENCRYPTED FILE-----\nquestion\n-----END AGE ENCRYPTED FILE-----\n");

    function setUp() public {
        token = new MockToken();
        box = new StumpBox(IERC20Like(address(token)), BURN_AMOUNT, sealer);
        token.mint(alice, 3 * BURN_AMOUNT);
        vm.prank(alice);
        token.approve(address(box), type(uint256).max);
        vm.prank(sealer);
        box.setDeadline(ID, uint64(block.timestamp + 1 hours));
    }

    function test_submitBurnsStraightToDeadAndEmitsTheCiphertext() public {
        vm.expectEmit(true, true, false, true, address(box));
        emit StumpBox.Submitted(ID, alice, keccak256(ct), ct);
        vm.prank(alice);
        box.submit(ID, ct);
        assertEq(token.balanceOf(box.BURN()), BURN_AMOUNT);
        assertEq(token.balanceOf(alice), 2 * BURN_AMOUNT);
        assertEq(token.balanceOf(address(box)), 0);
    }

    function test_closedBeforeDeadlineIsSetAndAfterItPasses() public {
        vm.prank(alice);
        vm.expectRevert(StumpBox.Closed.selector);
        box.submit(keccak256("other"), ct);
        vm.warp(block.timestamp + 1 hours);
        vm.prank(alice);
        vm.expectRevert(StumpBox.Closed.selector);
        box.submit(ID, ct);
    }

    function test_lengthBounds() public {
        vm.startPrank(alice);
        vm.expectRevert(StumpBox.BadLength.selector);
        box.submit(ID, "");
        vm.expectRevert(StumpBox.BadLength.selector);
        box.submit(ID, new bytes(4097));
        box.submit(ID, new bytes(4096));
        vm.stopPrank();
    }

    function test_noAllowanceMeansNoSubmission() public {
        address bob = address(0xB0B);
        token.mint(bob, BURN_AMOUNT);
        vm.prank(bob);
        vm.expectRevert(StumpBox.BurnFailed.selector);
        box.submit(ID, ct);
    }

    function test_tokenThatReturnsNothingStillBurns() public {
        SilentToken silent = new SilentToken();
        StumpBox b = new StumpBox(IERC20Like(address(silent)), BURN_AMOUNT, sealer);
        silent.mint(alice, BURN_AMOUNT);
        vm.prank(alice);
        silent.approve(address(b), BURN_AMOUNT);
        vm.prank(sealer);
        b.setDeadline(ID, uint64(block.timestamp + 1));
        vm.prank(alice);
        b.submit(ID, ct);
        assertEq(silent.balanceOf(b.BURN()), BURN_AMOUNT);
    }

    function test_tokenThatReturnsFalseIsRejected() public {
        FalseToken f = new FalseToken();
        StumpBox b = new StumpBox(IERC20Like(address(f)), BURN_AMOUNT, sealer);
        vm.prank(sealer);
        b.setDeadline(ID, uint64(block.timestamp + 1));
        vm.prank(alice);
        vm.expectRevert(StumpBox.BurnFailed.selector);
        b.submit(ID, ct);
    }

    function test_constructorChecks() public {
        vm.expectRevert(StumpBox.NotAContract.selector);
        new StumpBox(IERC20Like(address(0x1234)), BURN_AMOUNT, sealer);
        vm.expectRevert(StumpBox.ZeroValue.selector);
        new StumpBox(IERC20Like(address(token)), 0, sealer);
        vm.expectRevert(StumpBox.ZeroValue.selector);
        new StumpBox(IERC20Like(address(token)), BURN_AMOUNT, address(0));
    }

    function test_onlySealerSetsDeadlines() public {
        vm.prank(alice);
        vm.expectRevert(StumpBox.NotSealer.selector);
        box.setDeadline(ID, 0);
        vm.prank(sealer);
        box.setDeadline(ID, 0);
        vm.prank(alice);
        vm.expectRevert(StumpBox.Closed.selector);
        box.submit(ID, ct);
    }
}
