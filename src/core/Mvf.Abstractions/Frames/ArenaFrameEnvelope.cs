namespace Mvf.Abstractions.Frames;

/// <summary>
/// An <see cref="IFrameEnvelope"/> whose bytes live in the shared-memory <see cref="IDataPlane"/>.
/// The engine publishes a frame into the arena <b>once</b> — when the static graph says it fans out to
/// out-of-process consumers — and routes this envelope to them: a worker reads it by <see cref="Handle"/>
/// over stdio (zero copy), while an in-process consumer reads it via <see cref="OpenReadAsync"/>. The
/// descriptor (cameraId/sequence/contentType/…) is carried alongside the handle. Lifetime is
/// engine-owned: the executor releases the handle once every consumer of the frame has run.
/// </summary>
public sealed class ArenaFrameEnvelope : IFrameEnvelope
{
    private readonly IDataPlane _dataPlane;

    public ArenaFrameEnvelope(
        IDataPlane dataPlane,
        ArenaHandle handle,
        string cameraId,
        int sequenceNumber,
        string fileName,
        string contentType,
        DateTime timestampUtc,
        string? sourcePath)
    {
        _dataPlane = dataPlane;
        Handle = handle;
        CameraId = cameraId;
        SequenceNumber = sequenceNumber;
        FileName = fileName;
        ContentType = string.IsNullOrWhiteSpace(contentType) ? "application/octet-stream" : contentType;
        TimestampUtc = timestampUtc;
        SourcePath = sourcePath;
    }

    /// <summary>Copies the descriptor from <paramref name="source"/>; the bytes now live at <paramref name="handle"/>.</summary>
    public ArenaFrameEnvelope(IDataPlane dataPlane, ArenaHandle handle, IFrameEnvelope source)
        : this(dataPlane, handle, source.CameraId, source.SequenceNumber, source.FileName,
               source.ContentType, source.TimestampUtc, source.SourcePath)
    {
    }

    /// <summary>The location of this frame's bytes in the arena — hand this to a co-located worker.</summary>
    public ArenaHandle Handle { get; }

    /// <summary>The typed descriptor for the payload, read from the arena slot header.</summary>
    public PayloadDescriptor Descriptor =>
        _dataPlane.TryReadDescriptor(Handle, out var descriptor) ? descriptor : default;

    public string CameraId { get; }

    public int SequenceNumber { get; }

    public string FileName { get; }

    public string? SourcePath { get; }

    public DateTime TimestampUtc { get; }

    public string ContentType { get; }

    public long? ContentLength => Handle.Length;

    public ValueTask<Stream> OpenReadAsync(CancellationToken cancellationToken)
    {
        cancellationToken.ThrowIfCancellationRequested();
        return ValueTask.FromResult(_dataPlane.OpenRead(Handle));
    }
}
