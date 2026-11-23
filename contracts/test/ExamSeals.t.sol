// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

import {Test} from "forge-std/Test.sol";
import {ExamSeals} from "../src/ExamSeals.sol";

contract ExamSealsTest is Test {
    ExamSeals internal seals;
    address internal sealer = address(0x5EA1);
    bytes32 internal constant ID = keccak256("exam-1");
    bytes32 internal constant CT = keccak256("ciphertext");
    bytes32 internal constant PT = keccak256("plaintext");
    uint64 internal constant ROUND = 34172721; // opens 2026-11-22T04:25:27Z

    function setUp() public {
        seals = new ExamSeals(sealer);
        vm.warp(seals.roundTime(ROUND) - 1 days);
    }

    function test_eventSignaturesMatchTheCli() public pure {
        assertEq(ExamSeals.Sealed.selector, keccak256("Sealed(bytes32,bytes32,bytes32,uint64)"));
        assertEq(ExamSeals.Published.selector, keccak256("Published(bytes32,bytes32)"));
    }

    function test_roundTimeIsQuicknet() public view {
        assertEq(seals.roundTime(1), 1692803367);
        assertEq(seals.roundTime(ROUND), 1795321527);
    }

    function test_sealThenPublishAfterOpen() public {
        vm.expectEmit(true, false, false, true, address(seals));
        emit ExamSeals.Sealed(ID, CT, PT, ROUND);
        vm.prank(sealer);
        seals.seal(ID, CT, PT, ROUND);

        (bytes32 ct, bytes32 pt, uint64 round, uint64 sealedAt,,) = seals.seals(ID);
        assertEq(ct, CT);
        assertEq(pt, PT);
        assertEq(round, ROUND);
        assertEq(sealedAt, block.timestamp);

        vm.warp(seals.roundTime(ROUND));
        vm.expectEmit(true, false, false, true, address(seals));
        emit ExamSeals.Published(ID, bytes32(uint256(7)));
        vm.prank(sealer);
        seals.publish(ID, bytes32(uint256(7)));
        (,,,, bytes32 root, uint64 publishedAt) = seals.seals(ID);
        assertEq(root, bytes32(uint256(7)));
        assertEq(publishedAt, block.timestamp);
    }

    function test_onlySealerWrites() public {
        vm.expectRevert(ExamSeals.NotSealer.selector);
        seals.seal(ID, CT, PT, ROUND);
        vm.prank(sealer);
        seals.seal(ID, CT, PT, ROUND);
        vm.warp(seals.roundTime(ROUND));
        vm.expectRevert(ExamSeals.NotSealer.selector);
        seals.publish(ID, CT);
    }

    function test_cannotSealTwiceOrSealAnOpenRound() public {
        vm.startPrank(sealer);
        seals.seal(ID, CT, PT, ROUND);
        vm.expectRevert(ExamSeals.AlreadySealed.selector);
        seals.seal(ID, CT, PT, ROUND + 100);
        vm.warp(seals.roundTime(ROUND + 5));
        vm.expectRevert(ExamSeals.RoundAlreadyOpen.selector);
        seals.seal(keccak256("exam-2"), CT, PT, ROUND + 5);
        vm.stopPrank();
    }

    function test_publishRules() public {
        vm.startPrank(sealer);
        vm.expectRevert(ExamSeals.NotSealed.selector);
        seals.publish(ID, CT);
        seals.seal(ID, CT, PT, ROUND);
        vm.expectRevert(ExamSeals.RoundNotOpen.selector);
        seals.publish(ID, CT);
        vm.warp(seals.roundTime(ROUND));
        vm.expectRevert(ExamSeals.ZeroValue.selector);
        seals.publish(ID, bytes32(0));
        seals.publish(ID, CT);
        vm.expectRevert(ExamSeals.AlreadyPublished.selector);
        seals.publish(ID, PT);
        vm.stopPrank();
    }

    function test_zeroValuesRejected() public {
        vm.startPrank(sealer);
        vm.expectRevert(ExamSeals.ZeroValue.selector);
        seals.seal(bytes32(0), CT, PT, ROUND);
        vm.expectRevert(ExamSeals.ZeroValue.selector);
        seals.seal(ID, bytes32(0), PT, ROUND);
        vm.expectRevert(ExamSeals.ZeroValue.selector);
        seals.seal(ID, CT, bytes32(0), ROUND);
        vm.expectRevert(ExamSeals.ZeroValue.selector);
        seals.seal(ID, CT, PT, 0);
        vm.stopPrank();
        vm.expectRevert(ExamSeals.ZeroValue.selector);
        new ExamSeals(address(0));
    }

    function test_ownerRotatesSealerAndHandsOverInTwoSteps() public {
        vm.prank(address(0xBAD));
        vm.expectRevert(ExamSeals.NotOwner.selector);
        seals.setSealer(address(0xBAD));

        seals.setSealer(address(0xA11CE));
        assertEq(seals.sealer(), address(0xA11CE));
        vm.expectRevert(ExamSeals.ZeroValue.selector);
        seals.setSealer(address(0));

        seals.transferOwnership(address(0xB0B));
        assertEq(seals.owner(), address(this));
        vm.prank(address(0xBAD));
        vm.expectRevert(ExamSeals.NotPendingOwner.selector);
        seals.acceptOwnership();
        vm.prank(address(0xB0B));
        seals.acceptOwnership();
        assertEq(seals.owner(), address(0xB0B));
        assertEq(seals.pendingOwner(), address(0));
    }
}
