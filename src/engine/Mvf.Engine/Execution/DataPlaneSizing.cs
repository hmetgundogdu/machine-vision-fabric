using Mvf.Engine.Modules;
using Mvf.Graph.Pipelines;

namespace Mvf.Engine.Execution;

/// <summary>
/// Works out how many arena slots a pipeline actually needs. The serial executor keeps one frame in
/// flight, so a fixed slot count was always enough; the pipelined executor keeps a queue's worth per edge
/// and, once a node is replicated, several more per instance — which is why four instances exhaust the
/// old fixed arena and the run stops on backpressure. Slot count therefore has to come from the graph.
///
/// <para>Sized, not policed: a conservative pre-flight check would reject pipelines that run fine today,
/// because the worst case here is rarely reached all at once. This computes what to allocate.</para>
/// </summary>
public static class DataPlaneSizing
{
    /// <summary>Never allocate fewer than this — the historical default, so nothing regresses.</summary>
    public const int MinimumSlots = 8;

    /// <summary>
    /// Slots needed for <paramref name="definition"/>. Only nodes that run out-of-process hold arena
    /// buffers, and each of them can be holding, at once: its inbound edge queue
    /// (<paramref name="edgeQueueCapacity"/>), one frame per instance waiting in the work queue, one per
    /// instance executing, and up to one per instance parked in the reorder buffer — hence
    /// <c>queue + 3 × instances</c>. One more covers the frame a producer holds while publishing.
    /// </summary>
    public static int RequiredSlots(
        PipelineDefinition definition,
        ModuleCatalog? catalog,
        string integrationsRoot,
        int edgeQueueCapacity,
        bool pipelined)
    {
        ArgumentNullException.ThrowIfNull(definition);

        var loaded = catalog?.Load(integrationsRoot);
        var workerNodeIds = DataPlaneRouter.BuildWorkerNodeIds(definition, loaded);
        if (workerNodeIds.Count == 0)
        {
            return MinimumSlots;
        }

        // Serial keeps exactly one frame in flight regardless of the graph's shape.
        if (!pipelined)
        {
            return MinimumSlots;
        }

        var queue = Math.Max(1, edgeQueueCapacity);
        var total = 1;
        foreach (var node in definition.Nodes)
        {
            if (!workerNodeIds.Contains(node.Id))
            {
                continue;
            }

            var instances = node.Parallelism is { } requested && requested > 1 ? requested : 1;
            total += queue + (3 * instances);
        }

        return Math.Max(MinimumSlots, total);
    }
}
