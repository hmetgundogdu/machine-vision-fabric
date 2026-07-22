using System.Text.Json.Nodes;
using Mvf.Abstractions;
using Mvf.Abstractions.Frames;

namespace Mvf.Hosting.Worker;

/// <summary>
/// An <see cref="IFrameTransformer"/> backed by an out-of-process worker (e.g. Python): the worker
/// reads the input frame from the arena and writes a <b>new</b> frame back into the arena, so no bytes
/// cross the pipe. The engine reserves the output slot up front and passes its handle in the
/// <c>execute</c> message (input + output handles), so the child never allocates — the free-list stays
/// engine-side. The output is an arena-born <see cref="ArenaFrameEnvelope"/> whose lifetime the executor
/// then owns (it holds one producer reference until routed).
/// </summary>
public sealed class WorkerFrameTransformer(StdioWorkerProcess worker, IDataPlane dataPlane)
    : IFrameTransformer, ICheckpointable, IAsyncDisposable
{
    private int _requestId;

    public Task<byte[]?> CheckpointAsync(CancellationToken cancellationToken) =>
        WorkerCheckpoint.CheckpointAsync(worker, dataPlane, ++_requestId, cancellationToken);

    public Task RestoreAsync(ReadOnlyMemory<byte> state, CancellationToken cancellationToken) =>
        WorkerCheckpoint.RestoreAsync(worker, dataPlane, ++_requestId, state, cancellationToken);

    public async Task<IFrameEnvelope?> TransformAsync(IFrameEnvelope frame, CancellationToken cancellationToken)
    {
        var frameMessage = new JsonObject();
        var ownInputHandle = await WorkerFrameMarshal.AttachInputAsync(frameMessage, frame, dataPlane, cancellationToken);

        if (!dataPlane.TryReserve(out var outputHandle))
        {
            if (ownInputHandle is { } inputToRelease)
            {
                dataPlane.Release(inputToRelease);
            }

            throw new InvalidOperationException("The data plane has no free slot for the transformer's output.");
        }

        var request = new JsonObject
        {
            ["type"] = "execute",
            ["id"] = ++_requestId,
            ["frame"] = frameMessage,
            // Where the worker must write its output frame: [descriptor | payload], payload ≤ capacity.
            ["out"] = new JsonObject
            {
                ["offset"] = outputHandle.Offset,
                ["capacity"] = outputHandle.Length,
            },
        };

        JsonObject response;
        try
        {
            response = await worker.RequestAsync(request, cancellationToken);
        }
        catch
        {
            dataPlane.Release(outputHandle);
            throw;
        }
        finally
        {
            if (ownInputHandle is { } inputToRelease)
            {
                dataPlane.Release(inputToRelease);
            }
        }

        if ((string?)response["type"] == "error")
        {
            dataPlane.Release(outputHandle);
            throw new InvalidOperationException($"Worker transform failed: {(string?)response["message"]}");
        }

        // A null frame means the transformer dropped this one; reclaim the reserved slot.
        if (response["frame"] is not JsonObject)
        {
            dataPlane.Release(outputHandle);
            return null;
        }

        // The worker wrote a descriptor + payload into the reserved slot. Read it back and validate it —
        // a module's header is never trusted blindly (bounds, size, overflow).
        if (!dataPlane.TryReadDescriptor(outputHandle, out var descriptor)
            || !descriptor.TryValidate(dataPlane.SlotSize, out _))
        {
            dataPlane.Release(outputHandle);
            throw new InvalidOperationException("Transformer output has an invalid or oversized descriptor.");
        }

        // Hand back an arena-born frame sized to the actual payload; it carries one producer reference
        // that the executor drops once the frame is routed to its consumers.
        var finalizedHandle = new ArenaHandle(outputHandle.Offset, (int)descriptor.PayloadLength);
        return new ArenaFrameEnvelope(dataPlane, finalizedHandle, frame);
    }

    public ValueTask DisposeAsync() => worker.DisposeAsync();
}
