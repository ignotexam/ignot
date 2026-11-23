// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

interface IERC20Like {
    function transferFrom(address from, address to, uint256 amount) external returns (bool);
}

/// @title StumpBox
/// @notice Anyone may add a question to an upcoming exam by burning a fixed amount of the token.
/// The question arrives already timelock-encrypted by the submitter, so nobody can read it before the exam opens.
/// The contract never holds tokens: the burn goes straight from the submitter to the dead address.
contract StumpBox {
    address public constant BURN = 0x000000000000000000000000000000000000dEaD;
    uint256 public constant MAX_CIPHERTEXT = 4096;

    IERC20Like public immutable token;
    uint256 public immutable burnAmount;

    address public owner;
    address public pendingOwner;
    address public sealer;
    /// @notice Submissions for an exam close at this unix time; 0 means closed.
    mapping(bytes32 examId => uint64) public deadline;

    event Submitted(bytes32 indexed examId, address indexed submitter, bytes32 ciphertextHash, bytes ciphertext);
    event DeadlineSet(bytes32 indexed examId, uint64 deadline);
    event SealerChanged(address indexed previousSealer, address indexed newSealer);
    event OwnershipTransferStarted(address indexed previousOwner, address indexed newOwner);
    event OwnershipTransferred(address indexed previousOwner, address indexed newOwner);

    error NotOwner();
    error NotPendingOwner();
    error NotSealer();
    error ZeroValue();
    error NotAContract();
    error Closed();
    error BadLength();
    error BurnFailed();

    modifier onlyOwner() {
        if (msg.sender != owner) revert NotOwner();
        _;
    }

    modifier onlySealer() {
        if (msg.sender != sealer) revert NotSealer();
        _;
    }

    constructor(IERC20Like token_, uint256 burnAmount_, address initialSealer) {
        if (burnAmount_ == 0 || initialSealer == address(0)) revert ZeroValue();
        // A call to an address without code "succeeds" with no data, which would make every burn a silent no-op.
        if (address(token_).code.length == 0) revert NotAContract();
        token = token_;
        burnAmount = burnAmount_;
        owner = msg.sender;
        sealer = initialSealer;
        emit OwnershipTransferred(address(0), msg.sender);
        emit SealerChanged(address(0), initialSealer);
    }

    function setDeadline(bytes32 examId, uint64 newDeadline) external onlySealer {
        deadline[examId] = newDeadline;
        emit DeadlineSet(examId, newDeadline);
    }

    function submit(bytes32 examId, bytes calldata ciphertext) external {
        uint64 d = deadline[examId];
        if (d == 0 || block.timestamp >= d) revert Closed();
        if (ciphertext.length == 0 || ciphertext.length > MAX_CIPHERTEXT) revert BadLength();
        _burnFrom(msg.sender);
        emit Submitted(examId, msg.sender, keccak256(ciphertext), ciphertext);
    }

    /// Accepts tokens that return true and tokens that return nothing; rejects false and reverts.
    function _burnFrom(address from) private {
        (bool ok, bytes memory data) = address(token).call(abi.encodeCall(IERC20Like.transferFrom, (from, BURN, burnAmount)));
        if (!ok || (data.length != 0 && (data.length != 32 || !abi.decode(data, (bool))))) revert BurnFailed();
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
