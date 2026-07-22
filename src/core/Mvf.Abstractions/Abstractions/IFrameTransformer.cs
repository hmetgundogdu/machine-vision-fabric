namespace Mvf.Abstractions;

/// <summary>
/// Transforms a frame into a <b>new</b> frame — resize, normalize, crop, denoise, etc. Unlike a
/// filter (which passes the same frame through or drops it), a transformer produces fresh bytes.
/// Returns <c>null</c> to drop the frame this cycle.
///
/// <para>This is the data-producing counterpart to <see cref="IFrameClassifier"/> (which produces a
/// control signal). A worker-backed transformer writes its output straight into the shared-memory data
/// plane, so the new frame flows downstream zero-copy.</para>
/// </summary>
public interface IFrameTransformer
{
    Task<IFrameEnvelope?> TransformAsync(IFrameEnvelope frame, CancellationToken cancellationToken);
}
