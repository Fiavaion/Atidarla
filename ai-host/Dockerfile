# AI Host for a LAN or cloud GPU box (e.g. a Runpod pod): the engine connects
# to it over the `inline` transport. No model weights are baked in: mount the
# model cache at /models (PRD A61). Start with a token:
#
#   docker build -t atd-ai-host ai-host
#   docker run --gpus all -p 7420:7420 -e ATD_AI_HOST_TOKEN=... -v ~/atidarla-models:/models atd-ai-host
FROM nvidia/cuda:12.8.1-cudnn-runtime-ubuntu24.04

ENV DEBIAN_FRONTEND=noninteractive \
    PYTHONUNBUFFERED=1 \
    UV_COMPILE_BYTECODE=1 \
    UV_LINK_MODE=copy \
    UV_PYTHON_INSTALL_DIR=/opt/python \
    ATD_MODELS_DIR=/models

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
 && useradd --create-home atd \
 && mkdir -p /models && chown atd /models

ENV PATH=/app/.venv/bin:$PATH
USER atd
EXPOSE 7420
VOLUME /models
# Listening beyond loopback refuses to start without ATD_AI_HOST_TOKEN (or --token).
ENTRYPOINT ["atd-ai-host", "--listen", "0.0.0.0:7420"]
