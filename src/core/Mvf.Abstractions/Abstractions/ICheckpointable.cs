namespace Mvf.Abstractions;

/// <summary>
/// A module whose durable, cross-cycle state the engine can capture and restore — the basis for
/// resume-after-crash. State is serialized by the module itself (it knows its own shape) and travels
/// through the shared-memory data plane as a typed payload, never base64. The engine keeps the last
/// captured state so it can restore a worker that has been restarted.
///
/// <para><b>External resources are not state.</b> A camera socket, a model in GPU memory, a file or PLC
/// handle cannot be memcpy'd; on restore the module re-establishes them from its state (rehydration).
/// This is the one honest step a stateful module must implement.</para>
/// </summary>
public interface ICheckpointable
{
    /// <summary>
    /// Captures the module's current durable state, or <c>null</c> if it is stateless. Called at a
    /// cycle boundary (the engine is quiesced), so the snapshot is torn-free.
    /// </summary>
    Task<byte[]?> CheckpointAsync(CancellationToken cancellationToken);

    /// <summary>
    /// Restores previously captured state into a (typically freshly restarted) module, which then
    /// re-establishes any external resources. A no-op when <paramref name="state"/> is empty.
    /// </summary>
    Task RestoreAsync(ReadOnlyMemory<byte> state, CancellationToken cancellationToken);
}
