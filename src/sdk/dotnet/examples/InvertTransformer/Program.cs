// Sample worker transformer: inverts every byte of a frame (b -> 255 - b).
//
// The .NET twin of src/sdk/cpp/examples/invert_transformer.cpp and
// modules/py-invert-transformer. The input is read in place from the shared-memory arena and the
// output is written back into the engine's output slot; no bytes travel on the pipe.

using Mvf.Worker;

return WorkerModule.RunProcessor("dotnet.invert-transformer", (payload, _) =>
{
    var input = payload.Data;
    var inverted = new byte[input.Length];
    for (var i = 0; i < input.Length; i++)
    {
        inverted[i] = (byte)(255 - input[i]);
    }

    return Output.Blob(inverted);
});
