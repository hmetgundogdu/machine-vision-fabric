using Mvf.Abstractions;

namespace MachineVisionFabric.Integrations.SyntheticCamera;

/// <summary>
/// A frame that is an actual image rather than an opaque run of bytes: single-channel 8-bit pixels, in
/// row-major order, declaring its own <see cref="PayloadDescriptor"/>.
///
/// <para>Declaring the shape is the whole point. A consumer that only sees <c>N bytes</c> cannot tell a
/// 320x240 image from a vector of the same length, so it can neither render it nor reshape it safely —
/// it would have to guess. With the descriptor attached, everything downstream (a worker's numpy view, the
/// egress stream, the observer's preview) receives real pixels with real dimensions.</para>
/// </summary>
internal sealed class GrayscaleFrameEnvelope(
    string cameraId,
    int sequenceNumber,
    string fileName,
    byte[] pixels,
    int width,
    int height,
    DateTime timestampUtc) : IFrameEnvelope
{
    public string CameraId { get; } = cameraId;

    public int SequenceNumber { get; } = sequenceNumber;

    public string FileName { get; } = fileName;

    public string? SourcePath => null;

    public DateTime TimestampUtc { get; } = timestampUtc;

    /// <summary>Raw single-channel pixels — deliberately not an encoded format. Encoding belongs to whoever
    /// needs a file or a preview, not to the sensor.</summary>
    public string ContentType => "image/x-raw-gray8";

    public long? ContentLength => pixels.LongLength;

    /// <summary>Rows first, then columns — the row-major order every consumer in the stack assumes.</summary>
    public PayloadDescriptor? Descriptor =>
        new(PayloadMediaType.Image, PayloadElementType.UInt8, new long[] { height, width });

    public ValueTask<Stream> OpenReadAsync(CancellationToken cancellationToken) =>
        ValueTask.FromResult<Stream>(new MemoryStream(pixels, writable: false));
}
