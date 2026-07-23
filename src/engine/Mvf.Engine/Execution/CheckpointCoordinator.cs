using Mvf.Abstractions;

namespace Mvf.Engine.Execution;

/// <summary>
/// Captures the checkpointable runners' state and persists it, shared by both executors. The rule that
/// makes a capture valid is the same either way — it must run at a point where nothing is in flight —
/// but the two executors reach that point differently: serial is quiesced at every cycle boundary,
/// pipelined has to drain the stages first (the epoch barrier).
/// </summary>
internal static class CheckpointCoordinator
{
    /// <summary>
    /// Snapshots every runner that still reports state and, when a store is present, persists the whole
    /// accumulated set. A runner that reports no state is remembered in <paramref name="stateless"/> and
    /// skipped from then on; any failure is a warning, never fatal — losing a checkpoint must not take
    /// down a running pipeline.
    /// </summary>
    public static async Task CaptureAsync(
        IReadOnlyList<INodeRunner> runners,
        HashSet<string> stateless,
        Dictionary<string, byte[]> lastStates,
        ICheckpointStore? store,
        Action<string> warn,
        CancellationToken cancellationToken)
    {
        var changed = false;
        foreach (var runner in runners)
        {
            if (stateless.Contains(runner.NodeId) || runner is not ICheckpointable checkpointable)
            {
                continue;
            }

            try
            {
                var state = await checkpointable.CheckpointAsync(cancellationToken);
                if (state is null)
                {
                    stateless.Add(runner.NodeId);
                }
                else
                {
                    lastStates[runner.NodeId] = state;
                    changed = true;
                }
            }
            catch (Exception ex) when (ex is not OperationCanceledException)
            {
                warn($"Checkpoint failed for node '{runner.NodeId}': {ex.Message}");
            }
        }

        if (store is not null && changed && lastStates.Count > 0)
        {
            try
            {
                await store.SaveAsync(lastStates, cancellationToken);
            }
            catch (Exception ex) when (ex is not OperationCanceledException)
            {
                warn($"Checkpoint persist failed: {ex.Message}");
            }
        }
    }

    /// <summary>Restores a runner's persisted state, if it has any. A failure degrades to a warning.</summary>
    public static async Task RestoreAsync(
        INodeRunner runner,
        IReadOnlyDictionary<string, byte[]> restoredStates,
        Action<string> warn,
        CancellationToken cancellationToken)
    {
        if (runner is not ICheckpointable checkpointable || !restoredStates.TryGetValue(runner.NodeId, out var state))
        {
            return;
        }

        try
        {
            await checkpointable.RestoreAsync(state, cancellationToken);
        }
        catch (Exception ex) when (ex is not OperationCanceledException)
        {
            warn($"Restore failed for node '{runner.NodeId}': {ex.Message}");
        }
    }
}
