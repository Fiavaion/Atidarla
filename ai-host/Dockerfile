# AI Host for a LAN or cloud GPU box (e.g. a Runpod pod): the engine connects
# to it over the `inline` transport. No model weights are baked in: mount the
# model cache at /models (PRD A61). Start with a token:
#
#   docker build -t lsr-ai-host ai-host
#   docker run --gpus all -p 7420:7420 -e LSR_AI_HOST_TOKEN=... -v ~/lasair-models:/models lsr-ai-host
FROM nvidia/cuda:12.8.1-cudnn-runtime-ubuntu24.04

ENV DEBIAN_FRONTEND=noninteractive \
    PYTHONUNBUFFERED=1 \
    UV_COMPILE_BYTECODE=1 \
    UV_LINK_MODE=copy \
    UV_PYTHON_INSTALL_DIR=/opt/python \
    LSR_MODELS_DIR=/models

COPY --from=ghcr.io/astral-sh/uv:0.12.9 /uv /usr/local/bin/uv

WORKDIR /app
COPY pyproject.toml uv.lock README.md ./
COPY src ./src
# Linux torch wheels from PyPI bundle CUDA. ONNX Runtime's CUDA build is a
# different package, so swap it in for the CPU one the `vision` extra pulls.
RUN uv python install 3.13 \
 && uv sync --frozen --no-dev --extra diffusion --extra vision \
 && uv pip uninstall --python .venv/bin/python onnxruntime \
 && uv pip install --python .venv/bin/python onnxruntime-gpu \
 && useradd --create-home lsr \
 && mkdir -p /models && chown lsr /models

ENV PATH=/app/.venv/bin:$PATH
USER lsr
EXPOSE 7420
VOLUME /models
# Listening beyond loopback refuses to start without LSR_AI_HOST_TOKEN (or --token).
ENTRYPOINT ["lsr-ai-host", "--listen", "0.0.0.0:7420"]
