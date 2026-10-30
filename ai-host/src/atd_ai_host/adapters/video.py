"""`video.v2v`: video-to-video world models (interface only; backends arrive later).

A backend registers itself in `BACKENDS` under its model id prefix (e.g.
"decart"). Without an installed backend the adapter is not advertised and
`model.load` answers UNSUPPORTED.
"""

from __future__ import annotations

from collections.abc import Callable

from ..errors import UNSUPPORTED, RpcError
from ..frames import Frame
from . import Adapter, Progress

BACKENDS: dict[str, Callable[[str, str | None, dict, Progress], Adapter]] = {}


class VideoAdapter(Adapter):
    kind = "video.v2v"

    @classmethod
    def available(cls) -> bool:
        return bool(BACKENDS)

    def __init__(self, model_id: str, path: str | None, options: dict, progress: Progress) -> None:
        super().__init__(model_id, path, options, progress)
        backend = BACKENDS.get(model_id.split("/", 1)[0])
        if backend is None:
            raise RpcError(UNSUPPORTED, f"no video.v2v backend installed for {model_id!r}")
        self._impl = backend(model_id, path, options, progress)
        self.device = self._impl.device

    def new_state(self):
        return self._impl.new_state()

    def process(self, state, frame: Frame, params: dict) -> tuple[Frame, bool]:
        return self._impl.process(state, frame, params)

    def close(self) -> None:
        self._impl.close()
