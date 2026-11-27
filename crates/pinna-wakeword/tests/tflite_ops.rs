//! TFLite operators that openWakeWord classifiers use and tract-tflite doesn't
//! read on its own (`vendor/tract-tflite/PATCHES.md`). Each model here is built
//! in the test, so no downloaded model is needed.

use tract_onnx::prelude::*;
use tract_tflite::tflite as tfl;

/// A model with one operator over two float inputs of `[4]` and one output.
fn binary_op(op: tfl::BuiltinOperator) -> Vec<u8> {
    let mut b = flatbuffers::FlatBufferBuilder::new();
    let tensor = |b: &mut flatbuffers::FlatBufferBuilder<'static>, name: &str| {
        let shape = b.create_vector(&[4]);
        let name = b.create_string(name);
        tfl::Tensor::create(
            b,
            &tfl::TensorArgs {
                shape: Some(shape),
                type_: tfl::TensorType::FLOAT32,
                name: Some(name),
                ..Default::default()
            },
        )
    };
    let a = tensor(&mut b, "a");
    let c = tensor(&mut b, "b");
    let out = tensor(&mut b, "out");
    let tensors = b.create_vector(&[a, c, out]);
    let op_inputs = b.create_vector(&[0, 1]);
    let op_outputs = b.create_vector(&[2]);
    let operator = tfl::Operator::create(
        &mut b,
        &tfl::OperatorArgs {
            opcode_index: 0,
            inputs: Some(op_inputs),
            outputs: Some(op_outputs),
            ..Default::default()
        },
    );
    let operators = b.create_vector(&[operator]);
    let inputs = b.create_vector(&[0, 1]);
    let outputs = b.create_vector(&[2]);
    let graph = tfl::SubGraph::create(
        &mut b,
        &tfl::SubGraphArgs {
            tensors: Some(tensors),
            inputs: Some(inputs),
            outputs: Some(outputs),
            operators: Some(operators),
            ..Default::default()
        },
    );
    let graphs = b.create_vector(&[graph]);
    let code = tfl::OperatorCode::create(
        &mut b,
        &tfl::OperatorCodeArgs {
            deprecated_builtin_code: op.0 as i8,
            builtin_code: op,
            version: 1,
            ..Default::default()
        },
    );
    let codes = b.create_vector(&[code]);
    let empty = tfl::Buffer::create(&mut b, &tfl::BufferArgs::default());
    let buffers = b.create_vector(&[empty]);
    let model = tfl::Model::create(
        &mut b,
        &tfl::ModelArgs {
            version: 3,
            operator_codes: Some(codes),
            subgraphs: Some(graphs),
            buffers: Some(buffers),
            ..Default::default()
        },
    );
    tfl::finish_model_buffer(&mut b, model);
    b.finished_data().to_vec()
}

fn run(model: &[u8], a: [f32; 4], b: [f32; 4]) -> TractResult<Vec<f32>> {
    let plan = tract_tflite::tflite()
        .model_for_read(&mut &*model)?
        .into_optimized()?
        .into_runnable()?;
    let out = plan.run(tvec!(tensor1(&a).into_tvalue(), tensor1(&b).into_tvalue()))?;
    Ok(out[0].try_as_plain_ram()?.as_slice::<f32>()?.to_vec())
}

#[test]
fn squared_difference_is_the_square_of_a_subtraction() {
    // (a - b)², as a LayerNorm's variance uses it.
    let model = binary_op(tfl::BuiltinOperator::SQUARED_DIFFERENCE);
    let out = run(&model, [1.0, -2.0, 3.5, 0.0], [4.0, 1.0, 3.5, -0.5]).unwrap();
    assert_eq!(out, vec![9.0, 9.0, 0.0, 0.25]);
}
