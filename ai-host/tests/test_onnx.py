"""The `onnx` adapter against tiny models built here (nothing is downloaded)."""

import pytest

np = pytest.importorskip("numpy")
onnx = pytest.importorskip("onnx")
pytest.importorskip("onnxruntime")

from conftest import Client, RpcFailure, open_inline
from onnx import TensorProto, helper

from atd_ai_host.errors import UNSUPPORTED


def save_model(tmp_path, name, nodes, inputs, output, inits=()):
    graph = helper.make_graph(nodes, name, inputs, [output], initializer=list(inits))
    model = helper.make_model(graph, opset_imports=[helper.make_opsetid("", 17)])
    model.ir_version = 8
    onnx.checker.check_model(model)
    path = tmp_path / f"{name}.onnx"
    onnx.save(model, path)
    return str(path)


def image_input(name="x", channels=3, h="H", w="W"):
    return helper.make_tensor_value_info(name, TensorProto.FLOAT, [1, channels, h, w])


def run(host, path, task, data, w, h, params=None):
    c = Client(host.addr)
    open_inline(c)
    c.call(
        "model.load",
        {"model_id": path, "adapter": "onnx", "path": path, "options": {"providers": ["CPUExecutionProvider"]}},
    )
    result, out = c.call(
        "frame.submit",
        {"seq": 1, "model_id": path, "width": w, "height": h, "params": {"task": task, **(params or {})}},
        data,
    )
    return result, np.frombuffer(out, np.uint8).reshape(result["height"], result["width"], 4)


def frame(w, h):
    rng = np.random.default_rng(1)
    px = rng.integers(0, 256, (h, w, 4), dtype=np.uint8)
    px[..., 3] = 200
    return px


def test_generic_inverts_through_a_generated_model(host, tmp_path):
    one = helper.make_tensor("one", TensorProto.FLOAT, [], [1.0])
    path = save_model(
        tmp_path, "invert", [helper.make_node("Sub", ["one", "x"], ["y"])], [image_input()], image_input("y"), [one]
    )
    px = frame(8, 6)
    result, out = run(host, path, "generic", px.tobytes(), 8, 6)
    assert (result["width"], result["height"]) == (8, 6)
    assert np.abs(out[..., :3].astype(int) - (255 - px[..., :3].astype(int))).max() <= 1
    assert (out[..., 3] == 200).all()  # alpha passes through


def test_depth_is_grey_and_normalised(host, tmp_path):
    path = save_model(
        tmp_path,
        "depth",
        [helper.make_node("ReduceMean", ["x"], ["y"], keepdims=1, axes=[1])],
        [image_input()],
        helper.make_tensor_value_info("y", TensorProto.FLOAT, [1, 1, "H", "W"]),
    )
    _, out = run(host, path, "depth", frame(8, 8).tobytes(), 8, 8)
    assert (out[..., 0] == out[..., 1]).all() and (out[..., 1] == out[..., 2]).all()
    assert out[..., 0].min() == 0 and out[..., 0].max() == 255
    assert (out[..., 3] == 255).all()


def test_background_puts_the_matte_in_alpha(host, tmp_path):
    # Matte = sigmoid(mean channel) of a fixed-size input: exercises resizing too.
    path = save_model(
        tmp_path,
        "matte",
        [helper.make_node("ReduceMean", ["x"], ["m"], keepdims=1, axes=[1]), helper.make_node("Sigmoid", ["m"], ["y"])],
        [image_input(h=16, w=16)],
        helper.make_tensor_value_info("y", TensorProto.FLOAT, [1, 1, 16, 16]),
    )
    px = frame(32, 24)
    _, out = run(host, path, "background", px.tobytes(), 32, 24)
    assert (out[..., :3] == px[..., :3]).all()  # the input, with a new alpha
    assert out[..., 3].std() > 0


def test_upscale_returns_a_larger_frame(host, tmp_path):
    scales = helper.make_tensor("scales", TensorProto.FLOAT, [4], [1, 1, 2, 2])
    path = save_model(
        tmp_path,
        "up2",
        [helper.make_node("Resize", ["x", "", "scales"], ["y"], mode="nearest")],
        [image_input()],
        image_input("y", h="H2", w="W2"),
        [scales],
    )
    result, out = run(host, path, "upscale", frame(5, 4).tobytes(), 5, 4)
    assert (result["width"], result["height"]) == (10, 8) and out.shape == (8, 10, 4)


def test_pose_without_heatmaps_is_unsupported(host, tmp_path):
    path = save_model(
        tmp_path,
        "vec",
        [helper.make_node("ReduceMean", ["x"], ["y"], keepdims=0, axes=[2, 3])],
        [image_input()],
        helper.make_tensor_value_info("y", TensorProto.FLOAT, [1, 3]),
    )
    with pytest.raises(RpcFailure) as e:
        run(host, path, "pose", frame(8, 8).tobytes(), 8, 8)
    assert e.value.code == UNSUPPORTED
