//! The upload test (ADR 0013): `test_keyword` accepts the built-in models and
//! refuses a model that wakes on noise, is too slow or isn't a model.

mod common;

use common::*;
use pinna_wakeword::{test_keyword, test_keyword_within};
use prost::Message;
use tract_onnx::pb::{
    AttributeProto, GraphProto, ModelProto, NodeProto, OperatorSetIdProto, TensorProto,
    TensorShapeProto, TypeProto, ValueInfoProto,
    attribute_proto::AttributeType,
    tensor_proto::DataType,
    tensor_shape_proto::{self, Dimension},
    type_proto,
};

const THRESHOLD: f32 = 0.55;

fn model(name: &str) -> Vec<u8> {
    std::fs::read(model_dir().unwrap().join(name)).unwrap()
}

fn value_info(name: &str, dims: &[i64]) -> ValueInfoProto {
    ValueInfoProto {
        name: name.into(),
        r#type: Some(TypeProto {
            value: Some(type_proto::Value::TensorType(type_proto::Tensor {
                elem_type: DataType::Float as i32,
                shape: Some(TensorShapeProto {
                    dim: dims
                        .iter()
                        .map(|&d| Dimension {
                            value: Some(tensor_shape_proto::dimension::Value::DimValue(d)),
                            ..Default::default()
                        })
                        .collect(),
                }),
            })),
            ..Default::default()
        }),
        ..Default::default()
    }
}

/// A classifier from `[1, 16, 96]` to a score that is always 1.0.
fn always_on() -> Vec<u8> {
    let value = TensorProto {
        dims: vec![1, 1],
        data_type: DataType::Float as i32,
        float_data: vec![1.0],
        ..Default::default()
    };
    let node = NodeProto {
        op_type: "Constant".into(),
        output: vec!["score".into()],
        attribute: vec![AttributeProto {
            name: "value".into(),
            r#type: AttributeType::Tensor as i32,
            t: Some(value),
            ..Default::default()
        }],
        ..Default::default()
    };
    ModelProto {
        ir_version: 7,
        opset_import: vec![OperatorSetIdProto {
            domain: String::new(),
            version: 13,
        }],
        graph: Some(GraphProto {
            name: "always_on".into(),
            node: vec![node],
            input: vec![value_info("features", &[1, 16, 96])],
            output: vec![value_info("score", &[1, 1])],
            ..Default::default()
        }),
        ..Default::default()
    }
    .encode_to_vec()
}

#[test]
fn built_in_models_pass() {
    let Some(dir) = model_dir() else { return };
    for name in ["alexa_v0.1.onnx", "okay_nabu.tflite"] {
        let report =
            test_keyword(&dir, &model(name), THRESHOLD).unwrap_or_else(|e| panic!("{name}: {e}"));
        println!("{name}: {report:?}");
        assert!(report.peak_score < THRESHOLD);
        assert!(report.block_ms <= 8.0);
    }
}

#[test]
fn garbage_is_refused() {
    let Some(dir) = model_dir() else { return };
    let err = test_keyword(&dir, b"this is not a model at all", THRESHOLD).unwrap_err();
    assert!(
        err.to_string().contains("not an ONNX or TFLite model"),
        "{err}"
    );
}

#[test]
fn the_shared_models_are_named_as_such() {
    let Some(dir) = model_dir() else { return };
    // Owners pick these from openWakeWord's download by mistake.
    for name in ["melspectrogram.onnx", "embedding_model.onnx"] {
        let err = test_keyword(&dir, &model(name), THRESHOLD)
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("one of openWakeWord's shared models"),
            "{name}: {err}"
        );
    }
}

#[test]
fn a_model_that_wakes_on_noise_is_refused() {
    let Some(dir) = model_dir() else { return };
    let err = test_keyword(&dir, &always_on(), THRESHOLD)
        .unwrap_err()
        .to_string();
    assert!(
        err.starts_with("This model wakes on silence or noise"),
        "{err}"
    );
    assert!(err.contains("score 1.00"), "{err}");
}

#[test]
fn a_model_over_the_time_budget_is_refused() {
    let Some(dir) = model_dir() else { return };
    let err = test_keyword_within(&dir, &model("alexa_v0.1.onnx"), THRESHOLD, 0.0)
        .unwrap_err()
        .to_string();
    assert!(err.contains("ms for each 80 ms of audio"), "{err}");
    assert!(err.contains("at most 0 ms"), "{err}");
}
