"""JSON-RPC error codes of AI Host Protocol v1."""

from __future__ import annotations

PARSE_ERROR = -32700
INVALID_REQUEST = -32600
METHOD_NOT_FOUND = -32601
INVALID_PARAMS = -32602
INTERNAL_ERROR = -32603

VERSION_MISMATCH = -32001
UNKNOWN_MODEL = -32002
BAD_SLOT = -32003
STALE_FRAME = -32004
UNAUTHORIZED = -32005
BUSY = -32006
MODEL_LOAD_FAILED = -32007
UNSUPPORTED = -32008


class RpcError(Exception):
    def __init__(self, code: int, message: str) -> None:
        super().__init__(message)
        self.code = code
        self.message = message
