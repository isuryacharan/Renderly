//! Thin wrapper over `tract`: load an ONNX graph, pin its input shapes, optimise it once and run
//! it on `f32` tensors. Optimised plans are cached for the process lifetime because tract's
//! optimisation pass is by far the slowest step (tens to hundreds of ms for small graphs).

use super::models::{self, ModelError, ModelSource};
use std::collections::HashMap;
use std::io::Cursor;
use std::path::Path;
use std::sync::{Arc, Mutex, OnceLock};
use thiserror::Error;
use tract_onnx::prelude::*;

#[derive(Debug, Error)]
pub enum MlError {
    #[error("model: {0}")]
    Model(#[from] ModelError),
    #[error("failed to load ONNX graph: {0}")]
    Load(String),
    #[error("failed to optimise ONNX graph: {0}")]
    Optimise(String),
    #[error("inference failed: {0}")]
    Run(String),
    #[error("expected {expected} inputs, got {got}")]
    InputCount { expected: usize, got: usize },
    #[error(
        "input {index} has shape {got_shape:?} ({got} values) but the plan was built for {shape:?}"
    )]
    InputSize {
        index: usize,
        shape: Vec<usize>,
        got_shape: Vec<usize>,
        got: usize,
    },
    #[error("tensor shape {shape:?} needs {expected} values, got {got}")]
    TensorSize {
        shape: Vec<usize>,
        expected: usize,
        got: usize,
    },
}

/// A dense `f32` tensor in row-major order (NCHW for images).
#[derive(Debug, Clone, PartialEq)]
pub struct F32Tensor {
    pub shape: Vec<usize>,
    pub data: Vec<f32>,
}

impl F32Tensor {
    /// Build a tensor, checking that `data` matches `shape`.
    pub fn new(shape: Vec<usize>, data: Vec<f32>) -> Result<Self, MlError> {
        let expected: usize = shape.iter().product();
        if expected != data.len() {
            return Err(MlError::TensorSize {
                shape,
                expected,
                got: data.len(),
            });
        }
        Ok(Self { shape, data })
    }

    pub fn zeros(shape: &[usize]) -> Self {
        Self {
            shape: shape.to_vec(),
            data: vec![0.0; shape.iter().product()],
        }
    }
}

/// How one graph input is pinned when building a [`Plan`].
#[derive(Debug, Clone, PartialEq)]
pub enum InputFact {
    /// A runtime input of this fixed shape; supplied to [`Plan::run`] on every call.
    Shape(Vec<usize>),
    /// A compile-time constant (e.g. RVM's `downsample_ratio`). Folded into the optimised graph
    /// and *not* supplied to [`Plan::run`].
    Const(F32Tensor),
}

/// An optimised, runnable model with fixed input shapes. Cheap to share across threads.
pub struct Plan {
    model: Arc<TypedRunnableModel>,
    facts: Vec<InputFact>,
}

impl std::fmt::Debug for Plan {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Plan").field("inputs", &self.facts).finish()
    }
}

impl Plan {
    /// Build a plan from ONNX bytes with one fact per graph input, in graph order.
    pub fn from_bytes(bytes: &[u8], facts: &[InputFact]) -> Result<Self, MlError> {
        let mut reader = Cursor::new(bytes);
        let mut model = tract_onnx::onnx()
            .model_for_read(&mut reader)
            .map_err(|e| MlError::Load(format!("{e:#}")))?;
        for (i, fact) in facts.iter().enumerate() {
            let inference_fact: InferenceFact = match fact {
                InputFact::Shape(shape) => f32::fact(shape.as_slice()).into(),
                InputFact::Const(t) => {
                    let tensor = Tensor::from_shape(&t.shape, &t.data)
                        .map_err(|e| MlError::Load(format!("input {i}: {e:#}")))?;
                    InferenceFact::from(tensor)
                }
            };
            model = model
                .with_input_fact(i, inference_fact)
                .map_err(|e| MlError::Load(format!("input {i}: {e:#}")))?;
        }
        let plan = model
            .into_optimized()
            .map_err(|e| MlError::Optimise(format!("{e:#}")))?
            .into_runnable()
            .map_err(|e| MlError::Optimise(format!("{e:#}")))?;
        Ok(Self {
            model: plan,
            facts: facts.to_vec(),
        })
    }

    /// Convenience: every input is a runtime input with the given fixed shape.
    pub fn from_bytes_shapes(bytes: &[u8], shapes: &[Vec<usize>]) -> Result<Self, MlError> {
        let facts: Vec<InputFact> = shapes.iter().cloned().map(InputFact::Shape).collect();
        Self::from_bytes(bytes, &facts)
    }

    /// Build a plan from an ONNX file on disk.
    pub fn from_path(path: &Path, facts: &[InputFact]) -> Result<Self, MlError> {
        let bytes = std::fs::read(path).map_err(ModelError::Io)?;
        Self::from_bytes(&bytes, facts)
    }

    /// Shapes of the runtime (non-constant) inputs, in graph order.
    pub fn runtime_input_shapes(&self) -> Vec<&[usize]> {
        self.facts
            .iter()
            .filter_map(|f| match f {
                InputFact::Shape(s) => Some(s.as_slice()),
                InputFact::Const(_) => None,
            })
            .collect()
    }

    /// Run the model. `inputs` are the runtime inputs only (constants are already folded in), in
    /// graph order, each matching the shape the plan was built with.
    pub fn run(&self, inputs: &[F32Tensor]) -> Result<Vec<F32Tensor>, MlError> {
        let runtime = self.runtime_input_shapes();
        if inputs.len() != runtime.len() {
            return Err(MlError::InputCount {
                expected: runtime.len(),
                got: inputs.len(),
            });
        }
        let mut next = inputs.iter().enumerate();
        let mut feed: TVec<TValue> = TVec::new();
        for fact in &self.facts {
            let tensor = match fact {
                InputFact::Const(t) => Tensor::from_shape(&t.shape, &t.data),
                InputFact::Shape(shape) => {
                    let Some((index, inp)) = next.next() else {
                        return Err(MlError::InputCount {
                            expected: runtime.len(),
                            got: inputs.len(),
                        });
                    };
                    if inp.shape != *shape || inp.data.len() != shape.iter().product::<usize>() {
                        return Err(MlError::InputSize {
                            index,
                            shape: shape.clone(),
                            got_shape: inp.shape.clone(),
                            got: inp.data.len(),
                        });
                    }
                    Tensor::from_shape(shape, &inp.data)
                }
            }
            .map_err(|e| MlError::Run(format!("{e:#}")))?;
            feed.push(tensor.into());
        }
        let outs = self
            .model
            .run(feed)
            .map_err(|e| MlError::Run(format!("{e:#}")))?;
        outs.iter()
            .map(|t| {
                let view = t
                    .to_plain_array_view::<f32>()
                    .map_err(|e| MlError::Run(format!("{e:#}")))?;
                Ok(F32Tensor {
                    shape: view.shape().to_vec(),
                    data: view.iter().copied().collect(),
                })
            })
            .collect()
    }
}

type PlanCache = Mutex<HashMap<String, Arc<Plan>>>;

fn cache() -> &'static PlanCache {
    static CACHE: OnceLock<PlanCache> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Resolve manifest model `model_id` (env override -> bundled -> model cache; never downloads)
/// and return its optimised plan for `facts`, building it on first use. Plans are cached per
/// (model id, facts) for the lifetime of the process.
pub fn load_cached_plan(model_id: &str, facts: &[InputFact]) -> Result<Arc<Plan>, MlError> {
    let key = format!("{model_id}|{facts:?}");
    if let Some(plan) = cache().lock().ok().and_then(|c| c.get(&key).cloned()) {
        return Ok(plan);
    }
    let plan = Arc::new(match models::resolve_model(model_id)? {
        ModelSource::Bundled(bytes) => Plan::from_bytes(bytes, facts)?,
        ModelSource::File(path) => Plan::from_path(&path, facts)?,
    });
    if let Ok(mut c) = cache().lock() {
        c.insert(key, plan.clone());
    }
    Ok(plan)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tensor_new_checks_size() {
        assert!(F32Tensor::new(vec![2, 3], vec![0.0; 6]).is_ok());
        assert!(matches!(
            F32Tensor::new(vec![2, 3], vec![0.0; 5]),
            Err(MlError::TensorSize { .. })
        ));
    }

    #[test]
    fn rejects_garbage_bytes() {
        let err = Plan::from_bytes_shapes(b"not an onnx file", &[vec![1, 3, 8, 8]]).unwrap_err();
        assert!(matches!(err, MlError::Load(_)), "{err}");
    }

    #[test]
    fn unknown_model_id_is_an_error() {
        let err = load_cached_plan("no-such-model", &[]).unwrap_err();
        assert!(matches!(err, MlError::Model(ModelError::UnknownModel(_))));
    }
}
