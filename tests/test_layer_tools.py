import numpy as np

from astroimp.render import stretch
from astroimp.layer_tools import curve_lut, apply_curve, clip_coverage


def tone(**kwargs):
    return dict(black=0, white=1, stretch="asinh", k=10, intensity=1, **kwargs)


def test_invert_after_stretch_before_intensity():
    v = np.array([0, .03, .4, 1, 2], np.float32)
    layer = tone(invert=True)
    layer["intensity"] = .7
    expected = (1 - np.clip(np.arcsinh(10*v) / np.arcsinh(10), 0, 1)) * .7
    np.testing.assert_allclose(stretch(v, layer), expected, atol=1e-7)


def test_old_tone_without_invert_is_identical():
    v = np.linspace(0, 2, 101, dtype=np.float32)
    layer = tone()
    np.testing.assert_array_equal(stretch(v, layer), stretch(v, {**layer, "invert": False}))
    # Explicit identity is a no-op too, even above white when intensity brings tones back into range.
    layer["intensity"] = .4
    np.testing.assert_array_equal(stretch(v, layer), stretch(v, {**layer, "curve": [{"x": .4, "y": .4}]}))


def test_legacy_edits_render_identically_with_all_new_tools_defaulted():
    import copy
    from types import SimpleNamespace
    from test_assistant_edits import state, layer
    from astroimp.render import render
    from astroimp.edits import normalise
    old = state()
    old["layers"] = [layer("A", "a", stretch="asinh", k=13, intensity=.7)]
    old["global"]["stars"] = {"separate": False, "color": "layers"}
    plane = np.linspace(0, 2, 48, dtype=np.float32).reshape(6, 8)
    project = SimpleNamespace(stars=[], shape=plane.shape, keys=["a"], full=[plane], starless=[plane], lum=None,
                              meta={"width": 8, "height": 6, "stars": {"detect": False}, "layers": [{"key": "a"}]})
    before = copy.deepcopy(old)
    result, _ = render(project, old)
    expected = np.clip(np.arcsinh(13*plane) / np.arcsinh(13) * .7, 0, 1)
    np.testing.assert_allclose(result, np.repeat(expected[..., None], 3, axis=2), atol=1e-7)
    explicit = normalise(old)
    assert old == before
    assert explicit["layers"][0]["detail"]["amount"] == 0
    assert explicit["layers"][0]["clip"] is None
    np.testing.assert_array_equal(result, render(project, explicit)[0])


def test_detail_probe_uses_surrounding_tones_not_the_probe_boundary():
    import json
    import tempfile
    from pathlib import Path
    from test_fullres import synthetic
    from astroimp.render import Project, output, probe, render
    with tempfile.TemporaryDirectory(dir=Path.cwd()) as directory:
        session = Path(directory)
        edits, meta = synthetic(session)
        meta["stars"]["detect"] = False
        folder = session / "project"
        (folder / "meta.json").write_text(json.dumps(meta))
        plane = np.full((24, 32), .2, "<f2")
        plane[:, 17:] = .8
        for name in ("a.f16", "a.starless.f16"):
            plane.tofile(folder / name)
        edits["layers"][0]["detail"] = {"amount": 1.5, "radius": 6}
        edits["global"]["stars"] = {"separate": False, "color": "layers"}
        full = output(render(Project(folder), edits)[0], edits["global"])
        yy, xx = np.mgrid[:24, :32]
        selected = np.hypot(xx-16, yy-12) <= 1
        expected = np.round(full[selected].mean(0), 3)
        result = probe(folder, edits, {"x": 16, "y": 12, "radius": 1})
        np.testing.assert_allclose(result["final_rgb"], expected, atol=.001)


def test_curve_identity_endpoints_and_no_overshoot():
    np.testing.assert_allclose(curve_lut(), np.arange(256) / 255, atol=3e-8)
    points = [{"x": 0, "y": .1}, {"x": .3, "y": .7}, {"x": .6, "y": .7}, {"x": 1, "y": .9}]
    lut = curve_lut(points)
    assert lut[0] == np.float32(.1) and lut[-1] == np.float32(.9)
    assert np.all(np.diff(lut) >= 0)
    assert np.all(lut[77:153] == np.float32(.7))
    np.testing.assert_allclose(apply_curve(np.array([.1, .5, .9]), points),
                               np.interp([.1, .5, .9], np.arange(256)/255, lut), atol=1e-7)


def test_curve_follows_inversion_before_intensity():
    v = np.linspace(0, 1, 21, dtype=np.float32)
    layer = tone(invert=True, curve=[{"x": .4, "y": .7}])
    expected = apply_curve(1 - np.clip(np.arcsinh(10*v) / np.arcsinh(10), 0, 1), layer["curve"])
    np.testing.assert_allclose(stretch(v, layer), expected, atol=1e-7)


def test_detail_matches_separable_shader_kernel_and_export_scale():
    from scipy.ndimage import gaussian_filter
    yy, xx = np.mgrid[:61, :71]
    v = (.3 + .2*np.sin(xx / 3) + .1*np.cos(yy / 4)).astype(np.float32)
    for radius, scale in [(1, 1), (8, .5), (3, 2)]:
        sigma = radius * scale
        layer = tone(detail={"amount": 1.5, "radius": radius})
        base = np.clip(np.arcsinh(10*v) / np.arcsinh(10), 0, 1)
        # Literal mirror of detailBlurFragment: nearest boundary, 4-sigma kernel, two passes.
        n = int(np.floor(4*sigma+.5))
        weights = np.exp(-.5 * (np.arange(-n, n+1)/sigma)**2)
        weights /= weights.sum()
        blurred = base.copy()
        for axis in (1, 0):
            padded = np.pad(blurred, [(n, n) if a == axis else (0, 0) for a in range(2)], mode="edge")
            blurred = sum(w*np.take(padded, np.arange(base.shape[axis])+i, axis=axis)
                          for i, w in enumerate(weights))
        expected = np.clip(base + 1.5 * (base - blurred), 0, 1)
        np.testing.assert_allclose(stretch(v, layer, scale), expected, atol=1e-6)
        np.testing.assert_allclose(blurred, gaussian_filter(base, sigma, mode="nearest"), atol=1e-6)


def test_zero_detail_does_not_change_old_render():
    v = np.linspace(0, 3, 100, dtype=np.float32)
    layer = tone()
    np.testing.assert_array_equal(stretch(v, layer),
                                  stretch(v, {**layer, "detail": {"amount": 0, "radius": 64}, "curve": []}))


def test_crop_shader_math_rect_ellipse_and_scale_invariance():
    yy, xx = np.mgrid[:80, :120].astype(np.float32)
    xx, yy = (xx+.5)/120, (yy+.5)/80
    for shape in ("rect", "ellipse"):
        for feather in (0, .2, .5):
            clip = dict(x=.1, y=.2, w=.7, h=.6, feather=feather, shape=shape)
            actual = clip_coverage(xx, yy, clip, 120, 80)
            expected = np.empty_like(actual)
            for y in range(80):
                for x in range(120):
                    qx, qy = (xx[y, x]-.1)/.7, (yy[y, x]-.2)/.6
                    distance = ((1-np.hypot((qx-.5)*2, (qy-.5)*2))*24 if shape == "ellipse"
                                else min(qx*84, (1-qx)*84, qy*48, (1-qy)*48))
                    t = np.clip(distance / (feather*48), 0, 1) if feather else float(distance >= 0)
                    expected[y, x] = t*t*(3-2*t)
            np.testing.assert_allclose(actual, expected, atol=1e-6)
            np.testing.assert_allclose(actual, clip_coverage(xx, yy, clip, 240, 160), atol=1e-7)
            assert actual[0, 0] == 0
