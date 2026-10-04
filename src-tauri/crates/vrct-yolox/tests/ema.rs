use candle_core::{DType, Device, Tensor, Var};
use std::{collections::BTreeMap, path::Path};
use vrct_yolox::{ema::Ema, graph::Network};

fn network() -> Network {
    Network {
        device: Device::Cpu,
        variables: BTreeMap::from([
            (
                "weight".into(),
                Var::new(&[2f32, -4.], &Device::Cpu).unwrap(),
            ),
            (
                "bn.running_mean".into(),
                Var::new(&[3f32], &Device::Cpu).unwrap(),
            ),
            (
                "bn.running_var".into(),
                Var::new(&[7f32], &Device::Cpu).unwrap(),
            ),
        ]),
        nodes: vec![],
        output: 0,
        variant: "ema-test".into(),
        mixed_precision: false,
    }
}

fn set(network: &Network, name: &str, values: &[f32]) {
    network.variables[name]
        .set(&Tensor::new(values, &Device::Cpu).unwrap())
        .unwrap();
}

fn values(network: &Network, name: &str) -> Vec<f32> {
    network.variables[name].to_vec1::<f32>().unwrap()
}

fn checkpoint(path: &Path) -> BTreeMap<String, Tensor> {
    candle_core::safetensors::load(path, &Device::Cpu)
        .unwrap()
        .into_iter()
        .collect()
}

fn save_checkpoint(tensors: BTreeMap<String, Tensor>, path: &Path) {
    let tensors = tensors
        .into_iter()
        .collect::<std::collections::HashMap<_, _>>();
    candle_core::safetensors::save(&tensors, path).unwrap();
}

fn near(actual: &[f32], expected: &[f32]) {
    assert_eq!(actual.len(), expected.len());
    for (actual, expected) in actual.iter().zip(expected) {
        assert!((actual - expected).abs() < 1e-5, "{actual} != {expected}");
    }
}

#[test]
fn initialization_owns_snapshot_of_weights_and_bn_running_statistics() {
    let net = network();
    let ema = Ema::new(&net).unwrap();
    assert_eq!(ema.updates, 0);
    set(&net, "weight", &[50., 60.]);
    set(&net, "bn.running_mean", &[80.]);
    set(&net, "bn.running_var", &[90.]);
    ema.apply(&net).unwrap();
    assert_eq!(values(&net, "weight"), vec![2., -4.]);
    assert_eq!(values(&net, "bn.running_mean"), vec![3.]);
    assert_eq!(values(&net, "bn.running_var"), vec![7.]);
    // Applying the snapshot also must not alias the model's mutable storage.
    set(&net, "weight", &[100., 200.]);
    ema.apply(&net).unwrap();
    assert_eq!(values(&net, "weight"), vec![2., -4.]);
}

#[test]
fn first_and_second_updates_use_incremented_counter_and_correct_ramp() {
    let net = network();
    let mut ema = Ema::new(&net).unwrap();
    set(&net, "weight", &[10., 20.]);
    set(&net, "bn.running_mean", &[11.]);
    set(&net, "bn.running_var", &[15.]);
    ema.update(&net).unwrap();
    let decay1 = 0.9998 * (1. - (-1f64 / 2000.).exp());
    let first = [
        (2. * decay1 + 10. * (1. - decay1)) as f32,
        (-4. * decay1 + 20. * (1. - decay1)) as f32,
    ];
    let eval = network();
    ema.apply(&eval).unwrap();
    near(&values(&eval, "weight"), &first);
    near(
        &values(&eval, "bn.running_mean"),
        &[(3. * decay1 + 11. * (1. - decay1)) as f32],
    );
    near(
        &values(&eval, "bn.running_var"),
        &[(7. * decay1 + 15. * (1. - decay1)) as f32],
    );
    assert_eq!(values(&net, "weight"), vec![10., 20.]);
    set(&net, "weight", &[-5., 9.]);
    ema.update(&net).unwrap();
    let decay2 = 0.9998 * (1. - (-2f64 / 2000.).exp());
    ema.apply(&eval).unwrap();
    near(
        &values(&eval, "weight"),
        &[
            (first[0] as f64 * decay2 - 5. * (1. - decay2)) as f32,
            (first[1] as f64 * decay2 + 9. * (1. - decay2)) as f32,
        ],
    );
    assert_eq!(ema.updates, 2);
}

#[test]
fn roundtrip_resume_retains_counter_and_matches_uninterrupted_update() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("ema.safetensors");
    let net = network();
    let mut original = Ema::new(&net).unwrap();
    // Resume at a meaningful ramp point so resetting the counter cannot pass.
    original.updates = 1999;
    set(&net, "weight", &[30., -10.]);
    original.update(&net).unwrap();
    original.save(&path).unwrap();
    let saved = checkpoint(&path);
    assert_eq!(saved.len(), net.variables.len());
    assert!(saved.values().all(|tensor| tensor.dtype() == DType::F32));
    let mut resumed = Ema::load(&path, &net, original.updates).unwrap();
    assert_eq!(resumed.updates, 2000);
    assert_eq!(values(&net, "weight"), vec![30., -10.]);
    set(&net, "weight", &[5., 20.]);
    original.update(&net).unwrap();
    resumed.update(&net).unwrap();
    let uninterrupted = network();
    let restored = network();
    original.apply(&uninterrupted).unwrap();
    resumed.apply(&restored).unwrap();
    for name in net.variables.keys() {
        assert_eq!(values(&uninterrupted, name), values(&restored, name));
    }
    let decay = 0.9998 * (1. - (-2000f64 / 2000.).exp());
    near(
        &saved["weight"].to_vec1::<f32>().unwrap(),
        &[
            (2. * decay + 30. * (1. - decay)) as f32,
            (-4. * decay - 10. * (1. - decay)) as f32,
        ],
    );
    assert_eq!(resumed.updates, 2001);
}

#[test]
fn load_rejects_missing_extra_wrong_shape_and_non_f32_tensors() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("ema.safetensors");
    let net = network();
    Ema::new(&net).unwrap().save(&path).unwrap();
    let original = checkpoint(&path);
    for case in ["missing", "extra", "shape", "dtype"] {
        let mut tensors = original.clone();
        match case {
            "missing" => {
                tensors.remove("weight");
            }
            "extra" => {
                tensors.insert(
                    "unknown".into(),
                    Tensor::zeros(1, DType::F32, &Device::Cpu).unwrap(),
                );
            }
            "shape" => {
                tensors.insert(
                    "weight".into(),
                    Tensor::zeros((1, 2), DType::F32, &Device::Cpu).unwrap(),
                );
            }
            "dtype" => {
                tensors.insert(
                    "weight".into(),
                    Tensor::new(&[2f64, -4.], &Device::Cpu).unwrap(),
                );
            }
            _ => unreachable!(),
        }
        save_checkpoint(tensors, &path);
        assert!(Ema::load(&path, &net, 42).is_err(), "accepted {case}");
        assert_eq!(values(&net, "weight"), vec![2., -4.]);
    }
}

#[test]
fn load_rejects_nan_infinity_missing_files_and_malformed_checkpoint() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("ema.safetensors");
    let net = network();
    assert!(Ema::load(&path, &net, 0).is_err());
    for bad in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
        Ema::new(&net).unwrap().save(&path).unwrap();
        let mut tensors = checkpoint(&path);
        tensors.insert(
            "bn.running_var".into(),
            Tensor::new(&[bad], &Device::Cpu).unwrap(),
        );
        save_checkpoint(tensors, &path);
        let error = Ema::load(&path, &net, 0).err().unwrap().to_string();
        assert!(
            error.contains("non-finite") && error.contains("bn.running_var"),
            "{error}"
        );
    }
    std::fs::write(&path, b"not a safetensors file").unwrap();
    assert!(Ema::load(&path, &net, 0).is_err());
    assert_eq!(values(&net, "bn.running_var"), vec![7.]);
}

#[test]
fn incompatible_update_and_apply_leave_values_and_counter_unchanged() {
    let net = network();
    let mut ema = Ema::new(&net).unwrap();
    let mut other = network();
    other.variables.remove("weight");
    set(&other, "bn.running_mean", &[123.]);
    assert!(ema.update(&other).is_err());
    assert_eq!(ema.updates, 0);
    assert!(ema.apply(&other).is_err());
    assert_eq!(values(&other, "bn.running_mean"), vec![123.]);
    ema.apply(&net).unwrap();
    assert_eq!(values(&net, "weight"), vec![2., -4.]);
    other.variables.insert(
        "weight".into(),
        Var::new(&[1f32, 2., 3.], &Device::Cpu).unwrap(),
    );
    assert!(ema.update(&other).is_err());
    assert!(ema.apply(&other).is_err());
    assert_eq!(ema.updates, 0);
}

#[test]
fn counter_overflow_returns_error_without_changing_snapshot() {
    let net = network();
    let mut ema = Ema::new(&net).unwrap();
    ema.updates = usize::MAX;
    set(&net, "weight", &[10., 20.]);
    assert!(ema.update(&net).is_err());
    assert_eq!(ema.updates, usize::MAX);
    ema.apply(&net).unwrap();
    assert_eq!(values(&net, "weight"), vec![2., -4.]);
}

#[test]
fn float_parameters_convert_to_f32_masters_and_integer_parameters_fail() {
    let mut net = network();
    net.variables.insert(
        "weight".into(),
        Var::new(&[2f64, -4.], &Device::Cpu).unwrap(),
    );
    let ema = Ema::new(&net).unwrap();
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("ema.safetensors");
    ema.save(&path).unwrap();
    assert_eq!(checkpoint(&path)["weight"].dtype(), DType::F32);
    net.variables["weight"]
        .set(&Tensor::new(&[9f64, 10.], &Device::Cpu).unwrap())
        .unwrap();
    ema.apply(&net).unwrap();
    assert_eq!(
        net.variables["weight"].to_vec1::<f64>().unwrap(),
        vec![2., -4.]
    );
    net.variables
        .insert("weight".into(), Var::new(&[2u32, 4], &Device::Cpu).unwrap());
    assert!(Ema::new(&net).is_err());
    assert!(Ema::load(&path, &net, 0).is_err());
}

#[test]
fn non_finite_initialization_and_save_fail_clearly() {
    let net = network();
    let mut ema = Ema::new(&net).unwrap();
    set(&net, "weight", &[f32::NAN, 1.]);
    assert!(Ema::new(&net).is_err());
    ema.update(&net).unwrap();
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("ema.safetensors");
    assert!(ema.save(&path).is_err());
    assert!(!path.exists());
}
