"""`diffusion.sd` with real SD-Turbo weights (skipped unless they are in the model cache)."""

import pytest

torch = pytest.importorskip("torch")
pytest.importorskip("diffusers")
pytest.importorskip("transformers")
np = pytest.importorskip("numpy")

from lsr_ai_host.adapters.diffusion import DiffusionAdapter, Params, turbo_timesteps
from lsr_ai_host.errors import INVALID_PARAMS, UNSUPPORTED, RpcError
from lsr_ai_host.frames import Frame
from lsr_ai_host.paths import models_dir

MODEL = models_dir() / "sd-turbo"
needs_weights = pytest.mark.skipif(not (MODEL / "unet").is_dir(), reason=f"SD-Turbo not in {MODEL}")
PROMPT = "a watercolor painting of a lighthouse"


def test_turbo_timesteps():
    assert turbo_timesteps(1, 1.0) == [999]
    assert turbo_timesteps(1, 0.5) == [499]
    assert turbo_timesteps(2, 0.5) == [499, 249]
    assert turbo_timesteps(4, 1.0) == [999, 749, 499, 249]


def test_prompt_blend_params_merge_and_normalise():
    p = Params({"prompts": [{"text": "a", "weight": 1}, {"text": "b", "weight": 2}, {"text": "a", "weight": 1}]})
    assert p.blend == (("a", 0.5), ("b", 0.5))
    assert Params({"prompts": [{"text": "a", "weight": 0}, {"text": "b", "weight": 0}]}).blend == (
        ("a", 0.5),
        ("b", 0.5),
    )
    for bad in (
        {"steps": 5},
        {"rcfg": "maybe"},
        {"strength": 2},
        {"prompts": []},
        {"prompts": [{"text": "a", "weight": -1}]},
    ):
        with pytest.raises(RpcError) as e:
            Params(bad)
        assert e.value.code == INVALID_PARAMS


@pytest.fixture(scope="module")
def sd():
    a = DiffusionAdapter("sd-turbo", str(MODEL), {}, lambda stage, fraction: None)
    yield a
    a.close()


def make_frame(w=256, h=256, shift=0):
    yy, xx = np.mgrid[0:h, 0:w]
    px = np.stack([(xx + shift) % 256, yy % 256, (xx * yy) % 256, np.full_like(xx, 180)], -1).astype(np.uint8)
    return Frame(w, h, px.tobytes())


@needs_weights
def test_output_shape_and_fixed_seed_determinism(sd):
    params = {"prompts": [{"text": PROMPT, "weight": 1.0}], "seed": 42, "strength": 0.6, "steps": 2}
    frame = make_frame(300, 200)  # not multiples of 64: processed at 320x192, returned at 300x200
    runs = []
    for _ in range(2):
        st = sd.new_state()
        outs = [bytes(sd.process(st, frame, params)[0].data) for _ in range(3)]
        runs.append(outs)
    assert all(len(o) == 300 * 200 * 4 for o in runs[0])
    assert runs[0] == runs[1]
    px = np.frombuffer(runs[0][-1], np.uint8).reshape(200, 300, 4)
    assert (px[..., 3] == 180).all()  # alpha passes through
    assert px[..., :3].std() > 5  # an image, not a flat colour


@needs_weights
def test_prompt_blend_of_identical_prompts_equals_the_single_prompt(sd):
    single = sd.condition(Params({"prompts": [{"text": PROMPT, "weight": 1}]}).blend)
    blend = sd.condition(Params({"prompts": [{"text": PROMPT, "weight": 0.3}, {"text": PROMPT, "weight": 0.7}]}).blend)
    assert torch.equal(single, blend)
    two = Params({"prompts": [{"text": PROMPT, "weight": 1}, {"text": "a city at night", "weight": 1}]}).blend
    mixed = sd.condition(two).float()
    expected = (sd.embed(PROMPT).float() + sd.embed("a city at night").float()) / 2
    assert torch.allclose(mixed, expected, atol=1e-2)
    frame = make_frame()
    base = {"seed": 1, "strength": 0.5}
    a = sd.process(sd.new_state(), frame, {**base, "prompts": [{"text": PROMPT, "weight": 1}]})[0]
    b = sd.process(
        sd.new_state(), frame, {**base, "prompts": [{"text": PROMPT, "weight": 2}, {"text": PROMPT, "weight": 5}]}
    )[0]
    assert bytes(a.data) == bytes(b.data)


@needs_weights
@pytest.mark.parametrize("cfg", [{"rcfg": "self"}, {"rcfg": "onetime", "negative": "blurry"}, {"rcfg": "none"}])
def test_guidance_modes_run(sd, cfg):
    st = sd.new_state()
    params = {"prompts": [{"text": PROMPT, "weight": 1}], "steps": 2, "guidance": 1.5, **cfg}
    for _ in range(2):
        out, skipped = sd.process(st, make_frame(), params)
    assert not skipped and len(out.data) == 256 * 256 * 4


@needs_weights
def test_similarity_filter_skips_a_static_input(sd):
    st = sd.new_state()
    params = {"prompts": [{"text": PROMPT, "weight": 1}], "similarity": 0.95}
    first, skipped = sd.process(st, make_frame(), params)
    assert not skipped
    again, skipped = sd.process(st, make_frame(), params)
    assert skipped and bytes(again.data) == bytes(first.data)
    _, skipped = sd.process(st, make_frame(shift=128), params)
    assert not skipped  # a different frame is processed


@needs_weights
def test_lora_without_peft_is_unsupported(sd):
    import importlib.util

    if importlib.util.find_spec("peft") is not None:
        pytest.skip("peft is installed")
    with pytest.raises(RpcError) as e:
        sd.process(sd.new_state(), make_frame(), {"lora": [{"path": "/nope.safetensors", "weight": 1}]})
    assert e.value.code == UNSUPPORTED
