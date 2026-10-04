//! YOLOX exponential moving average of parameters and BatchNorm running statistics.
//! The update counter is stored by the training checkpoint alongside these tensors.

use crate::{graph::Network, Result};
use candle_core::{DType, Tensor};
use std::{collections::BTreeMap, path::Path};

pub struct Ema {
    pub updates: usize,
    tensors: BTreeMap<String, Tensor>,
}

impl Ema {
    pub fn new(network: &Network) -> Result<Self> {
        let mut tensors = BTreeMap::new();
        for (name, variable) in &network.variables {
            if !variable.dtype().is_float() {
                return Err(format!("EMA parameter {name} must be floating point").into());
            }
            // Detach alone aliases Var storage; training must not overwrite the snapshot.
            let tensor = variable.as_detached_tensor().to_dtype(DType::F32)?.copy()?;
            finite(name, &tensor)?;
            tensors.insert(name.clone(), tensor);
        }
        Ok(Self {
            updates: 0,
            tensors,
        })
    }

    pub fn load(path: impl AsRef<Path>, network: &Network, updates: usize) -> Result<Self> {
        let tensors: BTreeMap<_, _> = candle_core::safetensors::load(path, &network.device)?
            .into_iter()
            .collect();
        compatible(&tensors, network)?;
        for (name, tensor) in &tensors {
            if tensor.dtype() != DType::F32 {
                return Err(format!("EMA checkpoint parameter {name} must be F32").into());
            }
            finite(name, tensor)?;
        }
        Ok(Self { updates, tensors })
    }

    pub fn update(&mut self, network: &Network) -> Result<()> {
        compatible(&self.tensors, network)?;
        let updates = self
            .updates
            .checked_add(1)
            .ok_or("EMA update counter overflow")?;
        // YOLOX's warm-up ramp uses the incremented counter, including after resume.
        let decay = 0.9998 * (1. - (-(updates as f64) / 2000.).exp());
        let mut tensors = BTreeMap::new();
        for (name, previous) in &self.tensors {
            let current = network.variables[name]
                .as_detached_tensor()
                .to_device(previous.device())?
                .to_dtype(DType::F32)?;
            let next = previous
                .affine(decay, 0.)?
                .add(&current.affine(1. - decay, 0.)?)?;
            tensors.insert(name.clone(), next.detach());
        }
        // A failed conversion/allocation must leave both values and counter unchanged.
        self.tensors = tensors;
        self.updates = updates;
        Ok(())
    }

    /// Copy EMA values into an evaluation network. Use a separate network to retain live weights.
    pub fn apply(&self, network: &Network) -> Result<()> {
        compatible(&self.tensors, network)?;
        let ready = self
            .tensors
            .iter()
            .map(|(name, tensor)| {
                let variable = &network.variables[name];
                Ok((
                    variable,
                    tensor
                        .to_device(variable.device())?
                        .to_dtype(variable.dtype())?,
                ))
            })
            .collect::<Result<Vec<_>>>()?;
        for (variable, tensor) in ready {
            variable.set(&tensor)?;
        }
        Ok(())
    }

    pub fn save(&self, path: impl AsRef<Path>) -> Result<()> {
        for (name, tensor) in &self.tensors {
            finite(name, tensor)?;
        }
        let tensors = self
            .tensors
            .iter()
            .map(|(name, tensor)| (name.clone(), tensor.clone()))
            .collect::<std::collections::HashMap<_, _>>();
        candle_core::safetensors::save(&tensors, path)?;
        Ok(())
    }
}

fn compatible(tensors: &BTreeMap<String, Tensor>, network: &Network) -> Result<()> {
    for name in tensors.keys() {
        if !network.variables.contains_key(name) {
            return Err(format!("EMA has unexpected parameter {name}").into());
        }
    }
    for (name, variable) in &network.variables {
        let tensor = tensors
            .get(name)
            .ok_or_else(|| format!("EMA is missing parameter {name}"))?;
        if tensor.dims() != variable.dims() {
            return Err(format!(
                "EMA shape mismatch for {name}: {:?} versus {:?}",
                tensor.dims(),
                variable.dims()
            )
            .into());
        }
        if !variable.dtype().is_float() {
            return Err(format!("EMA parameter {name} must be floating point").into());
        }
    }
    Ok(())
}

fn finite(name: &str, tensor: &Tensor) -> Result<()> {
    if tensor
        .flatten_all()?
        .to_vec1::<f32>()?
        .iter()
        .any(|value| !value.is_finite())
    {
        return Err(format!("EMA has non-finite parameter {name}").into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use candle_core::{Device, Var};

    #[test]
    fn snapshots_and_updates_have_no_training_gradient() {
        let variable = Var::new(&[1f32, 2.], &Device::Cpu).unwrap();
        let network = Network {
            device: Device::Cpu,
            variables: BTreeMap::from([("weight".into(), variable.clone())]),
            nodes: vec![],
            output: 0,
            variant: "test".into(),
            mixed_precision: false,
        };
        let mut ema = Ema::new(&network).unwrap();
        for updated in [false, true] {
            if updated {
                ema.update(&network).unwrap();
            }
            let tensor = &ema.tensors["weight"];
            assert_eq!(tensor.dtype(), DType::F32);
            assert!(!tensor.is_variable());
            assert!(tensor
                .sum_all()
                .unwrap()
                .backward()
                .unwrap()
                .get(&variable)
                .is_none());
        }
    }
}
