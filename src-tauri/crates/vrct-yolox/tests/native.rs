use candle_core::{Device, Tensor};
use vrct_yolox::{
    evaluation::{self, Detection},
    network,
    training::{self, BoxLabel, TrainOptions},
};

#[test]
fn output_guards_preserve_source_files_and_hardlinks() {
    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("model.onnx");
    let alias = directory.path().join("alias.onnx");
    std::fs::write(&source, b"original model").unwrap();
    std::fs::hard_link(&source, &alias).unwrap();
    assert!(vrct_yolox::graph::protect_input(&source, &source).is_err());
    assert!(vrct_yolox::graph::protect_input(&alias, &source).is_err());
    assert!(vrct_yolox::graph::protect_input(&directory.path().join("new.onnx"), &source).is_ok());
    assert_eq!(std::fs::read(source).unwrap(), b"original model");
}

#[test]
fn detector_shapes_sigmoid_scores_and_backbone_gradient_are_real() {
    let net = network::build("tiny", Device::Cpu, 17).unwrap();
    let input = Tensor::from_vec(
        (0..3 * 64 * 64)
            .map(|v| ((v * 37) % 255) as f32)
            .collect::<Vec<_>>(),
        (1, 3, 64, 64),
        &Device::Cpu,
    )
    .unwrap();
    let prediction = net.forward(&input, true).unwrap();
    assert_eq!(prediction.dims(), &[1, 84, 6]);
    let loss = training::detection_loss(
        &prediction,
        &[vec![BoxLabel {
            cx: 31.,
            cy: 31.,
            w: 22.,
            h: 18.,
        }]],
        64,
    )
    .unwrap();
    assert!(loss.to_scalar::<f32>().unwrap().is_finite());
    let with_l1 = training::detection_loss_with_l1(
        &prediction,
        &[vec![BoxLabel {
            cx: 31.,
            cy: 31.,
            w: 22.,
            h: 18.,
        }]],
        64,
        true,
    )
    .unwrap();
    assert!(with_l1.to_scalar::<f32>().unwrap() > loss.to_scalar::<f32>().unwrap());
    let gradients = loss.backward().unwrap();
    let stem = &net.variables["backbone.backbone.stem.conv.conv.weight"];
    let gradient = gradients
        .get(stem)
        .expect("loss must reach backbone, not only the output head");
    assert!(
        gradient
            .abs()
            .unwrap()
            .sum_all()
            .unwrap()
            .to_scalar::<f32>()
            .unwrap()
            > 0.
    );
    let output = net
        .forward(&input, false)
        .unwrap()
        .to_vec3::<f32>()
        .unwrap();
    assert!(output[0]
        .iter()
        .all(|r| (0. ..=1.).contains(&r[4]) && (0. ..=1.).contains(&r[5])));
}
#[test]
fn nms_negative_images_and_ap_are_checked() {
    let rows = vec![
        vec![10., 10., 8., 8., 0.9, 0.9],
        vec![10., 10., 8., 8., 0.8, 0.8],
        vec![30., 30., 8., 8., 0.7, 0.7],
    ];
    let detections = evaluation::decode(&rows, 1., 0.15, 0.65).unwrap();
    assert_eq!(detections.len(), 2);
    let truth = vec![
        vec![BoxLabel {
            cx: 10.,
            cy: 10.,
            w: 8.,
            h: 8.,
        }],
        vec![],
    ];
    let perfect = vec![vec![detections[0].clone()], vec![]];
    assert!((evaluation::average_precision(&truth, &perfect, 0.5) - 1.).abs() < 1e-6);
    let with_negative = vec![
        perfect[0].clone(),
        vec![Detection {
            bbox: [0., 0., 3., 3.],
            score: 0.99,
        }],
    ];
    assert!((evaluation::average_precision(&truth, &with_negative, 0.5) - 0.5).abs() < 1e-6);
    assert!(evaluation::decode(&[vec![f32::NAN; 6]], 1., 0.15, 0.65).is_err());
}
#[test]
fn paths_cannot_escape_dataset_and_empty_labels_are_preserved() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("dataset");
    std::fs::create_dir_all(root.join("s/images")).unwrap();
    std::fs::create_dir_all(root.join("s/labels")).unwrap();
    let image = root.join("s/images/negative.png");
    image::RgbImage::new(32, 32).save(&image).unwrap();
    std::fs::write(root.join("s/labels/negative.txt"), "").unwrap();
    assert!(training::labels(&image).unwrap().is_empty());
    assert!(training::resolve(&root, "s/images/negative.png").is_ok());
    assert!(training::resolve(&root, "../outside.png").is_err());
    assert!(training::resolve(&root, &image.display().to_string()).is_err());
}
#[test]
fn native_train_writes_weights_optimizer_and_can_resume() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("dataset");
    std::fs::create_dir_all(root.join("s/images")).unwrap();
    std::fs::create_dir_all(root.join("s/labels")).unwrap();
    let mut image = image::RgbImage::new(64, 64);
    for (x, y, p) in image.enumerate_pixels_mut() {
        *p = image::Rgb([(x * 3) as u8, (y * 3) as u8, ((x + y) * 2) as u8]);
    }
    image.save(root.join("s/images/positive.png")).unwrap();
    std::fs::write(root.join("s/labels/positive.txt"), "0 0.5 0.5 0.25 0.25\n").unwrap();
    for split in ["train", "val"] {
        std::fs::write(
            root.join(format!("{split}.txt")),
            "./s/images/positive.png\n",
        )
        .unwrap();
    }
    let mut options = TrainOptions {
        root,
        output: dir.path().join("runs"),
        variant: "tiny".into(),
        checkpoint: None,
        resume: false,
        epochs: 1,
        batch_size: 1,
        size: 64,
        seed: 19,
        lr: 0.0001,
        warmup_epochs: 0,
        no_aug_epochs: 1,
        eval_interval: 1,
        device: "cpu".into(),
        fp16: false,
        multiscale_range: 0,
    };
    training::train(options.clone()).unwrap();
    let pointer: serde_json::Value =
        serde_json::from_slice(&std::fs::read(options.output.join("last.json")).unwrap()).unwrap();
    let checkpoint = options.output.join(pointer["checkpoint"].as_str().unwrap());
    assert!(checkpoint.join("weights.safetensors").is_file());
    assert!(checkpoint.join("momentum.safetensors").is_file());
    let trained = network::build("tiny", Device::Cpu, 19).unwrap();
    trained
        .load(&checkpoint.join("weights.safetensors"), false)
        .unwrap();
    let fresh = network::build("tiny", Device::Cpu, 19).unwrap();
    let weight = "backbone.backbone.stem.conv.conv.weight";
    assert_ne!(
        trained.variables[weight]
            .flatten_all()
            .unwrap()
            .to_vec1::<f32>()
            .unwrap(),
        fresh.variables[weight]
            .flatten_all()
            .unwrap()
            .to_vec1::<f32>()
            .unwrap()
    );
    options.checkpoint = Some(checkpoint);
    options.resume = true;
    options.epochs = 2;
    training::train(options.clone()).unwrap();
    let history: serde_json::Value =
        serde_json::from_slice(&std::fs::read(options.output.join("history.json")).unwrap())
            .unwrap();
    assert_eq!(history[0]["epoch"], 1);
    assert_eq!(history[1]["epoch"], 2);
    assert!(checkpoint_training_weights(&options).is_file());
    let mut invalid_precision = options;
    invalid_precision.fp16 = true;
    assert!(training::train(invalid_precision)
        .unwrap_err()
        .to_string()
        .contains("--device cuda"));
}

fn checkpoint_training_weights(options: &TrainOptions) -> std::path::PathBuf {
    let pointer: serde_json::Value =
        serde_json::from_slice(&std::fs::read(options.output.join("last.json")).unwrap()).unwrap();
    options
        .output
        .join(pointer["checkpoint"].as_str().unwrap())
        .join("training.safetensors")
}

#[test]
fn dynamic_onnx_matches_native_inference_and_quantization_runs() {
    use ort::{session::Session, value::Tensor as OrtTensor};
    let dir = tempfile::tempdir().unwrap();
    let model = dir.path().join("model.onnx");
    let net = network::build("tiny", Device::Cpu, 11).unwrap();
    net.export(&model, 64, 64, true).unwrap();
    evaluation::load_runtime().unwrap();
    let mut session = Session::builder()
        .unwrap()
        .with_intra_threads(1)
        .unwrap()
        .with_inter_threads(1)
        .unwrap()
        .commit_from_file(&model)
        .unwrap();
    for (height, width) in [(64, 64), (64, 96)] {
        let pixels = (0..3 * height * width)
            .map(|i| ((i * 23) % 255) as f32 / 255.)
            .collect::<Vec<_>>();
        let native = net
            .forward(
                &Tensor::from_vec(pixels.clone(), (1, 3, height, width), &Device::Cpu).unwrap(),
                false,
            )
            .unwrap()
            .flatten_all()
            .unwrap()
            .to_vec1::<f32>()
            .unwrap();
        let tensor = OrtTensor::from_array(([1usize, 3, height, width], pixels)).unwrap();
        let outputs = session.run(ort::inputs![tensor]).unwrap();
        let (_, onnx) = outputs[0].try_extract_tensor::<f32>().unwrap();
        assert_eq!(native.len(), onnx.len());
        let max = native
            .iter()
            .zip(onnx)
            .map(|(a, b)| (a - b).abs())
            .fold(0f32, f32::max);
        assert!(max < 0.003, "native vs ONNX max error {max}");
    }
    let root = dir.path().join("dataset");
    std::fs::create_dir_all(root.join("s/images")).unwrap();
    std::fs::create_dir_all(root.join("s/labels")).unwrap();
    image::RgbImage::from_pixel(64, 64, image::Rgb([10, 80, 150]))
        .save(root.join("s/images/negative.png"))
        .unwrap();
    std::fs::write(root.join("s/labels/negative.txt"), "").unwrap();
    std::fs::write(root.join("train.txt"), "./s/images/negative.png\n").unwrap();
    std::fs::write(root.join("val.txt"), "./s/images/negative.png\n").unwrap();
    let quantized = dir.path().join("quantized.onnx");
    vrct_yolox::quantization::quantize(&model, &quantized, &root, "train", 64, 64, 1).unwrap();
    let report =
        evaluation::evaluate_onnx(&quantized, &root, "val", "64,64", 0.15, 0.65, 0.5).unwrap();
    assert_eq!(report.images, 1);
    assert_eq!(report.missed, 0);
    assert!(
        std::fs::metadata(&quantized).unwrap().len() < std::fs::metadata(&model).unwrap().len()
    );
    assert!(
        vrct_yolox::quantization::quantize(&quantized, &quantized, &root, "train", 64, 64, 1)
            .is_err()
    );
}

#[test]
fn nano_is_depthwise_and_onnx_graph_is_decoded() {
    let net = network::build("nano", Device::Cpu, 0).unwrap();
    assert!(net
        .nodes
        .iter()
        .any(|n| matches!(n.op,vrct_yolox::graph::Op::Conv{groups,..}if groups>1)));
    let dir = tempfile::tempdir().unwrap();
    let model = dir.path().join("nano.onnx");
    net.export(&model, 64, 64, true).unwrap();
    let input = Tensor::from_vec(
        (0..3 * 64 * 64)
            .map(|i| (i % 255) as f32 / 255.)
            .collect::<Vec<_>>(),
        (1, 3, 64, 64),
        &Device::Cpu,
    )
    .unwrap();
    assert_eq!(net.forward(&input, false).unwrap().dims(), &[1, 84, 6]);
    evaluation::load_runtime().unwrap();
    let mut session = ort::session::Session::builder()
        .unwrap()
        .with_intra_threads(1)
        .unwrap()
        .commit_from_file(&model)
        .unwrap();
    let tensor = ort::value::Tensor::from_array((
        [1usize, 3, 64, 64],
        input.flatten_all().unwrap().to_vec1::<f32>().unwrap(),
    ))
    .unwrap();
    let result = session.run(ort::inputs![tensor]).unwrap();
    let (shape, _) = result[0].try_extract_tensor::<f32>().unwrap();
    assert_eq!(&**shape, &[1, 84, 6]);
}

#[test]
#[ignore = "Requires the official Apache-2.0 YOLOX COCO checkpoint in native-cache"]
fn imports_official_coco_weights_without_python() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/native-cache/yolox/yolox_tiny_coco.pth");
    let net = network::build("tiny", Device::Cpu, 7).unwrap();
    let loaded = net.load(&path, true).unwrap();
    assert!(
        loaded > 300,
        "must load the whole pretrained backbone/head, not just a handful of tensors"
    );
    let input = Tensor::from_vec(
        (0..3 * 64 * 64)
            .map(|i| (i % 255) as f32)
            .collect::<Vec<_>>(),
        (1, 3, 64, 64),
        &Device::Cpu,
    )
    .unwrap();
    let result = net
        .forward(&input, false)
        .unwrap()
        .to_vec3::<f32>()
        .unwrap();
    assert!(result[0].iter().flatten().all(|v| v.is_finite()));
    assert!(
        net.load(&path, false).is_err(),
        "80-class checkpoint must not masquerade as a complete single-class detector"
    );
}
