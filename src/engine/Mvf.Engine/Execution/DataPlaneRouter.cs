using Mvf.Abstractions;
using Mvf.Abstractions.Frames;
using Mvf.Engine.Modules;
using Mvf.Graph.Execution;
using Mvf.Graph.Pipelines;

namespace Mvf.Engine.Execution;

/// <summary>
/// Hands one routed value to one downstream edge. The serial executor stores it in the per-cycle port
/// bus; the pipelined executor writes it into that edge's bounded channel, where a full channel blocks
/// — which is why this is asynchronous.
/// </summary>
internal delegate ValueTask DeliverToEdge(
    PipelineEdgeDefinition edge, PortValue value, CancellationToken cancellationToken);

/// <summary>
/// Graph-aware transport selection and <b>live-edge-occupancy</b> reference counting, shared by both
/// executors — a buffer's refcount equals the number of edges currently carrying it, whether an edge is
/// a port-bus slot (serial) or a queued message (pipelined).
/// </summary>
internal static class DataPlaneRouter
{
    /// <summary>Node ids whose module runs out-of-process (a non-<c>dotnet</c> runtime in the catalog).</summary>
    public static IReadOnlySet<string> BuildWorkerNodeIds(
        PipelineDefinition definition,
        IReadOnlyDictionary<string, ModuleCatalogEntry>? catalog)
    {
        var workers = new HashSet<string>(StringComparer.OrdinalIgnoreCase);
        if (catalog is null)
        {
            return workers;
        }

        foreach (var node in definition.Nodes)
        {
            if (node.ModuleId is not null
                && catalog.TryGetValue(node.ModuleId, out var entry)
                && !string.Equals(entry.Manifest.Runtime, "dotnet", StringComparison.OrdinalIgnoreCase))
            {
                workers.Add(node.Id);
            }
        }

        return workers;
    }

    public static Dictionary<string, List<PipelineEdgeDefinition>> BuildOutgoingByPort(PipelineDefinition definition)
    {
        var map = new Dictionary<string, List<PipelineEdgeDefinition>>(StringComparer.OrdinalIgnoreCase);
        foreach (var edge in definition.Edges)
        {
            var key = PortKey(edge.From.NodeId, edge.From.Port);
            if (!map.TryGetValue(key, out var list))
            {
                list = [];
                map[key] = list;
            }

            list.Add(edge);
        }

        return map;
    }

    // '\0' separator (written as an escape rather than the raw byte the serial executor used, which made
    // the file read as binary): no node id or port name can contain it, so ("a", "b c") and ("a b", "c")
    // can never collide on one key.
    public static string PortKey(string nodeId, string port) => $"{nodeId}\0{port}";

    /// <summary>
    /// Routes a node's outputs, choosing the transport per edge from the static graph:
    /// <list type="bullet">
    ///   <item><b>Heap frame</b> that fans out to workers: published into the arena once (refcount =
    ///   worker edges); the arena handle goes to workers, in-process consumers keep the heap frame.</item>
    ///   <item><b>Arena frame</b> (a worker's output, or a pass-through node re-emitting an arena input):
    ///   every consumer reads the arena, so it is <see cref="IDataPlane.AddRef"/>'d by the number of
    ///   outgoing edges and delivered to all of them.</item>
    ///   <item>Control signals and in-process-only frames are routed by reference, unchanged.</item>
    /// </list>
    /// A newly produced arena buffer (not one of this node's inputs) carries a producer hold from
    /// reservation; once routed, that hold is dropped so its refcount is exactly its live edge count.
    /// AddRef runs here (Phase 1), before the caller releases this node's arena inputs (Phase 2), so a
    /// forwarded buffer never transiently reaches zero.
    /// </summary>
    /// <returns>How many frames were dropped for worker consumers under <see cref="BackpressurePolicy.Drop"/>.</returns>
    public static async Task<int> RouteAsync(
        string sourceNodeId,
        NodeExecutionResult result,
        NodeExecutionInputs inputs,
        DeliverToEdge deliver,
        IReadOnlyDictionary<string, List<PipelineEdgeDefinition>> outgoingByPort,
        IReadOnlySet<string> workerNodeIds,
        IDataPlane dataPlane,
        BackpressurePolicy policy,
        CancellationToken cancellationToken)
    {
        var dropped = 0;

        // Arena buffers this node received as inputs — an output carrying one of these is a forwarded
        // pass-through, not a newly produced buffer, so it keeps no producer hold to drop.
        var inputArenaHandles = new HashSet<ArenaHandle>();
        foreach (var (_, value) in inputs.All)
        {
            if (value.Frame is ArenaFrameEnvelope arenaInput)
            {
                inputArenaHandles.Add(arenaInput.Handle);
            }
        }

        var producedArenaHandles = new HashSet<ArenaHandle>();

        foreach (var (portName, value) in result.All)
        {
            var edges = outgoingByPort.TryGetValue(PortKey(sourceNodeId, portName), out var e) ? e : null;

            if (value.Frame is ArenaFrameEnvelope arenaFrame)
            {
                // Arena-born or forwarded: every consumer reads the arena in place.
                var edgeCount = edges?.Count ?? 0;
                dataPlane.AddRef(arenaFrame.Handle, edgeCount);
                if (edges is not null)
                {
                    foreach (var edge in edges)
                    {
                        await deliver(edge, value, cancellationToken);
                    }
                }

                producedArenaHandles.Add(arenaFrame.Handle);
                continue;
            }

            if (edges is null)
            {
                continue;
            }

            if (value.Frame is not null)
            {
                var workerEdgeCount = edges.Count(edge => workerNodeIds.Contains(edge.To.NodeId));
                if (workerEdgeCount > 0)
                {
                    var (outcome, arenaValue) = await TryPublishFrameForWorkersAsync(
                        value.Frame, workerEdgeCount, dataPlane, cancellationToken);

                    if (outcome == PublishOutcome.Published)
                    {
                        foreach (var edge in edges)
                        {
                            var payload = workerNodeIds.Contains(edge.To.NodeId) ? arenaValue! : value;
                            await deliver(edge, payload, cancellationToken);
                        }

                        // Published with refcount == worker-edge count (no producer hold), so each worker
                        // release balances it — this buffer is not tracked for a producer-hold drop.
                        continue;
                    }

                    if (outcome == PublishOutcome.PayloadTooLarge)
                    {
                        // Not backpressure: a frame that never fits a slot can't be fixed by waiting or
                        // by dropping every frame forever. Stop with an actionable message under any policy.
                        throw new DataPlaneBackpressureException(
                            $"Frame on '{sourceNodeId}.{portName}' exceeds the arena slot capacity " +
                            $"({dataPlane.SlotSize} bytes) and can never be published — increase the slot size.");
                    }

                    // Arena momentarily full → the lossless-vs-lossy choice.
                    if (policy == BackpressurePolicy.Stall)
                    {
                        throw new DataPlaneBackpressureException(
                            $"Data plane full while publishing '{sourceNodeId}.{portName}' to {workerEdgeCount} " +
                            "worker edge(s); a lossless (stall) run cannot proceed — raise the arena slot count " +
                            "or set the backpressure policy to drop.");
                    }

                    // Drop: out-of-process consumers miss this frame; in-process consumers on the same
                    // output still receive the heap frame. The source keeps running (bounded latency).
                    foreach (var edge in edges)
                    {
                        if (!workerNodeIds.Contains(edge.To.NodeId))
                        {
                            await deliver(edge, value, cancellationToken);
                        }
                    }

                    dropped++;
                    continue;
                }
            }

            foreach (var edge in edges)
            {
                await deliver(edge, value, cancellationToken);
            }
        }

        // Drop the producer hold on buffers born at this node (a worker's output). Forwarded input
        // buffers keep their occupancy and are released in Phase 2 instead.
        foreach (var handle in producedArenaHandles)
        {
            if (!inputArenaHandles.Contains(handle))
            {
                dataPlane.Release(handle);
            }
        }

        return dropped;
    }

    /// <summary>Releases one reference for each arena-backed frame a node consumed, now that it has run.</summary>
    public static void ReleaseArenaInputs(NodeExecutionInputs inputs, IDataPlane dataPlane)
    {
        foreach (var (_, value) in inputs.All)
        {
            if (value.Frame is ArenaFrameEnvelope arenaFrame)
            {
                dataPlane.Release(arenaFrame.Handle);
            }
        }
    }

    /// <summary>Why a publish attempt did not place a frame in the arena.</summary>
    private enum PublishOutcome
    {
        /// <summary>The frame is in the arena; the returned value carries its handle.</summary>
        Published,

        /// <summary>The arena is momentarily full — every slot carries a live buffer (backpressure).</summary>
        ArenaFull,

        /// <summary>The frame is larger than a slot and can never be published (a sizing error).</summary>
        PayloadTooLarge
    }

    /// <summary>
    /// Copies a heap frame into the arena once (refcount = its worker-edge count) and, on failure,
    /// classifies whether the arena was merely full (backpressure) or the frame can never fit a slot.
    /// The caller turns that classification into the configured policy.
    /// </summary>
    private static async Task<(PublishOutcome Outcome, PortValue? Value)> TryPublishFrameForWorkersAsync(
        IFrameEnvelope frame,
        int referenceCount,
        IDataPlane dataPlane,
        CancellationToken cancellationToken)
    {
        byte[] bytes;
        await using (var stream = await frame.OpenReadAsync(cancellationToken))
        using (var buffer = new MemoryStream())
        {
            await stream.CopyToAsync(buffer, cancellationToken);
            bytes = buffer.ToArray();
        }

        // An encoded frame is an opaque byte blob (u8, length N); its media type/decoding is the
        // consumer's concern. Raw tensors get a richer descriptor when those payload types land.
        var descriptor = new PayloadDescriptor(PayloadMediaType.Blob, PayloadElementType.UInt8, [bytes.Length]);
        if (dataPlane.TryPublish(descriptor, bytes, referenceCount, out var handle))
        {
            return (PublishOutcome.Published, PortValue.FromFrame(new ArenaFrameEnvelope(dataPlane, handle, frame)));
        }

        // A payload that can never fit a slot is a permanent sizing error, not transient backpressure.
        var tooLarge = PayloadDescriptor.HeaderSize + (long)bytes.Length > dataPlane.SlotSize;
        return (tooLarge ? PublishOutcome.PayloadTooLarge : PublishOutcome.ArenaFull, null);
    }
}

/// <summary>
/// Thrown when a producer cannot place a frame in the arena and the run cannot continue: a lossless
/// (stall) policy meeting an exhausted arena, or a frame that can never fit a slot. Caught by the
/// executor and turned into a failed report with an actionable message.
/// </summary>
internal sealed class DataPlaneBackpressureException(string message) : Exception(message);

/// <summary>Which end of the graph a node sits at. Shared so both executors classify nodes identically.</summary>
internal static class NodeRoles
{
    public static bool IsSource(PipelineNodeDefinition node) =>
        string.Equals(node.Category, "source", StringComparison.OrdinalIgnoreCase)
        || (string.Equals(node.Kind, "runtime-builtin", StringComparison.OrdinalIgnoreCase)
            && string.Equals(node.BuiltinType, "folder-sequence-source", StringComparison.OrdinalIgnoreCase));

    public static bool IsSink(PipelineNodeDefinition node) =>
        string.Equals(node.Category, "output", StringComparison.OrdinalIgnoreCase)
        || string.Equals(node.Category, "sink", StringComparison.OrdinalIgnoreCase);
}
