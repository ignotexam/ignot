// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

/// @title ExamSeals
/// @notice Public record that an exam was sealed before its drand round opened, and of its results after.
/// Holds no funds and has no upgrade path. The sealer writes; the owner can only replace the sealer.
contract ExamSeals {
    /// drand quicknet: first round at this unix time, one round every 3 seconds.
    uint64 public constant DRAND_GENESIS = 1692803367;
    uint64 public constant DRAND_PERIOD = 3;

    struct Seal {
        bytes32 ciphertextHash;
        bytes32 plaintextHash;
        uint64 drandRound;
        uint64 sealedAt;
        bytes32 resultsRoot;
        uint64 publishedAt;
    }

    address public owner;
    address public pendingOwner;
    address public sealer;
    mapping(bytes32 examId => Seal) public seals;

    event Sealed(bytes32 indexed examId, bytes32 ciphertextHash, bytes32 plaintextHash, uint64 drandRound);
    event Published(bytes32 indexed examId, bytes32 resultsRoot);
    event SealerChanged(address indexed previousSealer, address indexed newSealer);
    event OwnershipTransferStarted(address indexed previousOwner, address indexed newOwner);
    event OwnershipTransferred(address indexed previousOwner, address indexed newOwner);

    error NotOwner();
    error NotPendingOwner();
    error NotSealer();
    error ZeroValue();
    error AlreadySealed();
    error NotSealed();
    error AlreadyPublished();
    error RoundAlreadyOpen();
    error RoundNotOpen();

    modifier onlyOwner() {
        if (msg.sender != owner) revert NotOwner();
        _;
    }

    modifier onlySealer() {
        if (msg.sender != sealer) revert NotSealer();
        _;
    }

    constructor(address initialSealer) {
        if (initialSealer == address(0)) revert ZeroValue();
        owner = msg.sender;
        sealer = initialSealer;
        emit OwnershipTransferred(address(0), msg.sender);
        emit SealerChanged(address(0), initialSealer);
    }

    /// @notice Unix time at which the beacon for `round` is published.
    function roundTime(uint64 round) public pure returns (uint64) {
        if (round == 0) revert ZeroValue();
        return DRAND_GENESIS + (round - 1) * DRAND_PERIOD;
    }

    /// @notice Record a sealed exam. Only possible while its round is still in the future.
    function seal(bytes32 examId, bytes32 ciphertextHash, bytes32 plaintextHash, uint64 drandRound) external onlySealer {
        if (examId == bytes32(0) || ciphertextHash == bytes32(0) || plaintextHash == bytes32(0)) revert ZeroValue();
        if (seals[examId].sealedAt != 0) revert AlreadySealed();
        if (block.timestamp >= roundTime(drandRound)) revert RoundAlreadyOpen();
        seals[examId] = Seal(ciphertextHash, plaintextHash, drandRound, uint64(block.timestamp), bytes32(0), 0);
        emit Sealed(examId, ciphertextHash, plaintextHash, drandRound);
    }

    /// @notice Record the results root once. Only possible after the exam's round has opened.
    function publish(bytes32 examId, bytes32 resultsRoot) external onlySealer {
        Seal storage s = seals[examId];
        if (s.sealedAt == 0) revert NotSealed();
        if (s.publishedAt != 0) revert AlreadyPublished();
        if (resultsRoot == bytes32(0)) revert ZeroValue();
        if (block.timestamp < roundTime(s.drandRound)) revert RoundNotOpen();
        s.resultsRoot = resultsRoot;
        s.publishedAt = uint64(block.timestamp);
        emit Published(examId, resultsRoot);
    }

    function setSealer(address newSealer) external onlyOwner {
        if (newSealer == address(0)) revert ZeroValue();
        emit SealerChanged(sealer, newSealer);
        sealer = newSealer;
    }

    function transferOwnership(address newOwner) external onlyOwner {
        pendingOwner = newOwner;
        emit OwnershipTransferStarted(owner, newOwner);
    }

    function acceptOwnership() external {
        if (msg.sender != pendingOwner) revert NotPendingOwner();
        emit OwnershipTransferred(owner, msg.sender);
        owner = msg.sender;
        pendingOwner = address(0);
    }
}
