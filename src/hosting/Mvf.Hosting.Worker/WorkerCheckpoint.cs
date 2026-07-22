using System.Text.Json.Nodes;
using Mvf.Abstractions;

namespace Mvf.Hosting.Worker;

/// <summary>
/// Checkpoint/restore of a worker's durable state over the shared-memory data plane (no base64),
/// shared by every worker-backed runner. Checkpoint hands the child a reserved slot to write its state
/// into; restore publishes the last captured state and points the child at it. See
/// <c>protocol/README.md</c>.
/// </summary>
internal static class WorkerCheckpoint
{
    public static async Task<byte[]?> CheckpointAsync(
        StdioWorkerProcess worker,
        IDataPlane dataPlane,
        int requestId,
        CancellationToken cancellationToken)
    {
        if (!dataPlane.TryReserve(out var slot))
        {
            throw new InvalidOperationException("The data plane has no free slot for a checkpoint.");
        }

        JsonObject response;
        try
        {
            var request = new JsonObject
            {
                ["type"] = "checkpoint",
                ["id"] = requestId,
                ["out"] = new JsonObject { ["offset"] = slot.Offset, ["capacity"] = slot.Length },
            };
            response = await worker.RequestAsync(request, cancellationToken);
        }
        catch
        {
            dataPlane.Release(slot);
            throw;
        }

        try
        {
            if ((string?)response["type"] == "error")
            {
                throw new InvalidOperationException($"Worker checkpoint failed: {(string?)response["message"]}");
            }

            // A stateless module reports empty; nothing was written to the slot.
            if (response["empty"] is JsonValue emptyValue && emptyValue.GetValue<bool>())
            {
                return null;
            }

            if (!dataPlane.TryReadDescriptor(slot, out var descriptor)
                || !descriptor.TryValidate(dataPlane.SlotSize, out _))
            {
                throw new InvalidOperationException("Checkpoint produced an invalid or oversized descriptor.");
            }

            var handle = new ArenaHandle(slot.Offset, (int)descriptor.PayloadLength);
            await using var stream = dataPlane.OpenRead(handle);
            using var buffer = new MemoryStream();
            await stream.CopyToAsync(buffer, cancellationToken);
            return buffer.ToArray();
        }
        finally
        {
            dataPlane.Release(slot);
        }
    }

    public static async Task RestoreAsync(
        StdioWorkerProcess worker,
        IDataPlane dataPlane,
        int requestId,
        ReadOnlyMemory<byte> state,
        CancellationToken cancellationToken)
    {
        if (state.IsEmpty)
        {
            return;
        }

        var descriptor = new PayloadDescriptor(PayloadMediaType.Blob, PayloadElementType.UInt8, [state.Length]);
        if (!dataPlane.TryPublish(descriptor, state.Span, referenceCount: 1, out var handle))
        {
            throw new InvalidOperationException(
                $"Restore state of {state.Length} bytes could not be published (slot capacity {dataPlane.SlotSize}).");
        }

        try
        {
            var request = new JsonObject
            {
                ["type"] = "restore",
                ["id"] = requestId,
                ["shm"] = new JsonObject { ["offset"] = handle.Offset },
            };
            var response = await worker.RequestAsync(request, cancellationToken);
            if ((string?)response["type"] == "error")
            {
                throw new InvalidOperationException($"Worker restore failed: {(string?)response["message"]}");
            }
        }
        finally
        {
            dataPlane.Release(handle);
        }
    }
}
