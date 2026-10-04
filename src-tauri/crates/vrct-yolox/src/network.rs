//! CSPDarknet, PAN FPN and decoupled YOLOX heads, following Megvii YOLOX.
use crate::graph::{Network, Op};
use crate::Result;
use candle_core::Device;
use rand::SeedableRng;
use rand_chacha::ChaCha20Rng;
use std::collections::BTreeMap;

struct Builder {
    net: Network,
    rng: ChaCha20Rng,
    depthwise: bool,
}
impl Builder {
    // These are the concrete Conv2d dimensions and grouping, kept beside each layer.
    #[allow(clippy::too_many_arguments)]
    fn base(
        &mut self,
        name: &str,
        x: usize,
        input: usize,
        output: usize,
        k: usize,
        stride: usize,
        groups: usize,
    ) -> Result<usize> {
        let weight = format!("{name}.conv.weight");
        self.net.parameter(
            &weight,
            &[output, input / groups, k, k],
            &mut self.rng,
            None,
        )?;
        let y = self.net.node(
            format!("/{name}/Conv"),
            &[x],
            Op::Conv {
                weight,
                bias: None,
                stride,
                groups,
            },
        );
        let bn = format!("{name}.bn");
        for (suffix, value) in [
            ("weight", 1.),
            ("bias", 0.),
            ("running_mean", 0.),
            ("running_var", 1.),
        ] {
            self.net.parameter(
                &format!("{bn}.{suffix}"),
                &[output],
                &mut self.rng,
                Some(value),
            )?;
        }
        let y = self.net.node(
            format!("/{name}/BatchNormalization"),
            &[y],
            Op::BatchNorm { name: bn },
        );
        Ok(self.net.node(format!("/{name}/Silu"), &[y], Op::Silu))
    }
    fn conv(
        &mut self,
        name: &str,
        x: usize,
        input: usize,
        output: usize,
        k: usize,
        stride: usize,
    ) -> Result<usize> {
        if self.depthwise && k > 1 {
            let y = self.base(&format!("{name}.dconv"), x, input, input, k, stride, input)?;
            self.base(&format!("{name}.pconv"), y, input, output, 1, 1, 1)
        } else {
            self.base(name, x, input, output, k, stride, 1)
        }
    }
    fn csp(
        &mut self,
        name: &str,
        x: usize,
        input: usize,
        output: usize,
        n: usize,
        shortcut: bool,
    ) -> Result<usize> {
        let hidden = output / 2;
        let mut a = self.base(&format!("{name}.conv1"), x, input, hidden, 1, 1, 1)?;
        let b = self.base(&format!("{name}.conv2"), x, input, hidden, 1, 1, 1)?;
        for i in 0..n {
            let y = self.base(&format!("{name}.m.{i}.conv1"), a, hidden, hidden, 1, 1, 1)?;
            let y = self.conv(&format!("{name}.m.{i}.conv2"), y, hidden, hidden, 3, 1)?;
            a = if shortcut {
                self.net
                    .node(format!("/{name}/m.{i}/Add"), &[a, y], Op::Add)
            } else {
                y
            };
        }
        let y = self.net.node(format!("/{name}/Concat"), &[a, b], Op::Cat);
        self.base(&format!("{name}.conv3"), y, 2 * hidden, output, 1, 1, 1)
    }
    fn prediction(
        &mut self,
        name: &str,
        x: usize,
        channels: usize,
        output: usize,
        bias_value: f32,
    ) -> Result<usize> {
        let weight = format!("{name}.weight");
        let bias = format!("{name}.bias");
        self.net
            .parameter(&weight, &[output, channels, 1, 1], &mut self.rng, None)?;
        self.net
            .parameter(&bias, &[output], &mut self.rng, Some(bias_value))?;
        Ok(self.net.node(
            format!("/{name}/Conv"),
            &[x],
            Op::Conv {
                weight,
                bias: Some(bias),
                stride: 1,
                groups: 1,
            },
        ))
    }
}
pub fn build(variant: &str, device: Device, seed: u64) -> Result<Network> {
    let width = match variant {
        "tiny" => 0.375,
        "nano" => 0.25,
        _ => return Err("variant must be tiny or nano".into()),
    };
    let channels = |c: usize| (c as f64 * width) as usize;
    let base = channels(64);
    let mut b = Builder {
        net: Network {
            device,
            variables: BTreeMap::new(),
            nodes: Vec::new(),
            output: 0,
            variant: variant.into(),
            mixed_precision: false,
        },
        rng: ChaCha20Rng::seed_from_u64(seed),
        depthwise: variant == "nano",
    };
    let mut slices = Vec::new();
    // Focus order matches YOLOX: top-left, bottom-left, top-right, bottom-right.
    for (i, (y, x)) in [(0, 0), (1, 0), (0, 1), (1, 1)].into_iter().enumerate() {
        slices.push(b.net.node(
            format!("/backbone/backbone/stem/Slice{i}"),
            &[0],
            Op::Slice { y, x },
        ));
    }
    let x = b
        .net
        .node("/backbone/backbone/stem/Concat", &slices, Op::Cat);
    let mut x = b.base("backbone.backbone.stem.conv", x, 12, base, 3, 1, 1)?;
    let mut features = Vec::new();
    for stage in 2..=5 {
        let input = base * (1 << (stage - 2));
        let output = input * 2;
        let prefix = format!("backbone.backbone.dark{stage}");
        x = b.conv(&format!("{prefix}.0"), x, input, output, 3, 2)?;
        if stage == 5 {
            let hidden = output / 2;
            let y = b.base(&format!("{prefix}.1.conv1"), x, output, hidden, 1, 1, 1)?;
            let mut pooled = vec![y];
            for k in [5, 9, 13] {
                pooled.push(b.net.node(
                    format!("/{prefix}.1/MaxPool{k}"),
                    &[y],
                    Op::Pool { kernel: k },
                ));
            }
            let y = b.net.node(format!("/{prefix}.1/Concat"), &pooled, Op::Cat);
            x = b.base(&format!("{prefix}.1.conv2"), y, 4 * hidden, output, 1, 1, 1)?;
            x = b.csp(&format!("{prefix}.2"), x, output, output, 1, false)?;
        } else {
            x = b.csp(
                &format!("{prefix}.1"),
                x,
                output,
                output,
                if stage == 2 { 1 } else { 3 },
                true,
            )?;
        }
        if stage >= 3 {
            features.push(x);
        }
    }
    let [c3, c4, c5] = [channels(256), channels(512), channels(1024)];
    let lateral = b.base("backbone.lateral_conv0", features[2], c5, c4, 1, 1, 1)?;
    let up = b.net.node("/backbone/upsample0", &[lateral], Op::Upsample);
    let cat = b
        .net
        .node("/backbone/topdown0", &[up, features[1]], Op::Cat);
    let top = b.csp("backbone.C3_p4", cat, c4 * 2, c4, 1, false)?;
    let reduced = b.base("backbone.reduce_conv1", top, c4, c3, 1, 1, 1)?;
    let up = b.net.node("/backbone/upsample1", &[reduced], Op::Upsample);
    let cat = b
        .net
        .node("/backbone/topdown1", &[up, features[0]], Op::Cat);
    let small = b.csp("backbone.C3_p3", cat, c3 * 2, c3, 1, false)?;
    let down = b.conv("backbone.bu_conv2", small, c3, c3, 3, 2)?;
    let cat = b.net.node("/backbone/bottomup0", &[down, reduced], Op::Cat);
    let medium = b.csp("backbone.C3_n3", cat, c3 * 2, c4, 1, false)?;
    let down = b.conv("backbone.bu_conv1", medium, c4, c4, 3, 2)?;
    let cat = b.net.node("/backbone/bottomup1", &[down, lateral], Op::Cat);
    let large = b.csp("backbone.C3_n4", cat, c4 * 2, c5, 1, false)?;
    let mut heads = Vec::new();
    let hidden = channels(256);
    for (i, ((x, c), stride)) in [small, medium, large]
        .into_iter()
        .zip([c3, c4, c5])
        .zip([8, 16, 32])
        .enumerate()
    {
        let stem = b.base(&format!("head.stems.{i}"), x, c, hidden, 1, 1, 1)?;
        let mut cls = stem;
        let mut reg = stem;
        for j in 0..2 {
            cls = b.conv(
                &format!("head.cls_convs.{i}.{j}"),
                cls,
                hidden,
                hidden,
                3,
                1,
            )?;
            reg = b.conv(
                &format!("head.reg_convs.{i}.{j}"),
                reg,
                hidden,
                hidden,
                3,
                1,
            )?;
        }
        let bias = -(99f32).ln();
        let xywh = b.prediction(&format!("head.reg_preds.{i}"), reg, hidden, 4, 0.)?;
        let obj = b.prediction(&format!("head.obj_preds.{i}"), reg, hidden, 1, bias)?;
        let cls = b.prediction(&format!("head.cls_preds.{i}"), cls, hidden, 1, bias)?;
        let raw = b
            .net
            .node(format!("/head/Concat{i}"), &[xywh, obj, cls], Op::Cat);
        heads.push(
            b.net
                .node(format!("/head/Decode{i}"), &[raw], Op::Head { stride }),
        );
    }
    b.net.output = b.net.node("/head/Outputs", &heads, Op::Outputs);
    Ok(b.net)
}
