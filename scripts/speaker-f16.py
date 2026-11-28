# Store the speaker model's weights as float16, for the image (ADR 0038).
#
# Each float32 initializer becomes a float16 one, with a Cast back to float32
# ahead of the graph. tract folds those casts when it loads the model, so it
# still computes in float32: the file halves (28 MB to 15 MB) and the
# voiceprints stay the same (cosine 0.999995 against the float32 model on the
# wake word test recordings, 2026-11-28). Small tensors stay float32.
#
#   python speaker-f16.py <float32 model> <float16 model>
#
# scripts/fetch-models.sh runs it with pinned onnx and numpy versions, and
# checks the result's sha256.
import sys

import numpy as np
import onnx
from onnx import TensorProto, helper, numpy_helper

src, dst = sys.argv[1], sys.argv[2]
model = onnx.load(src)
graph = model.graph
casts, initializers = [], []
for init in graph.initializer:
    if init.data_type == TensorProto.FLOAT and int(np.prod(init.dims or [1])) >= 16:
        half = numpy_helper.from_array(numpy_helper.to_array(init).astype(np.float16), init.name + "__f16")
        initializers.append(half)
        casts.append(helper.make_node("Cast", [half.name], [init.name], to=TensorProto.FLOAT, name=init.name + "__cast"))
    else:
        initializers.append(init)
del graph.initializer[:]
graph.initializer.extend(initializers)
nodes = list(graph.node)
del graph.node[:]
graph.node.extend(casts + nodes)
onnx.checker.check_model(model)
onnx.save(model, dst)
