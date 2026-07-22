namespace Mvf.Abstractions;

/// <summary>
/// Durably persists the captured state of a run's checkpointable nodes so a run interrupted by an
/// engine/process crash can resume. State is keyed by node id and stored as raw bytes (no base64). A
/// clean completion clears the store; an interrupted run leaves it for the next start to reload.
/// </summary>
public interface ICheckpointStore
{
    /// <summary>Persists the given per-node states, replacing any previous capture (best-effort atomic).</summary>
    Task SaveAsync(IReadOnlyDictionary<string, byte[]> statesByNodeId, CancellationToken cancellationToken);

    /// <summary>Loads previously persisted per-node states, or an empty map when there is no checkpoint.</summary>
    Task<IReadOnlyDictionary<string, byte[]>> LoadAsync(CancellationToken cancellationToken);

    /// <summary>Removes the persisted checkpoint (called after a clean, fully-consumed run).</summary>
    Task ClearAsync(CancellationToken cancellationToken);
}
