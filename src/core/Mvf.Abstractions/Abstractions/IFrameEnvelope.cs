namespace Mvf.Abstractions;

public interface IFrameEnvelope
{
    string CameraId { get; }

    int SequenceNumber { get; }

    string FileName { get; }

    string? SourcePath { get; }

    DateTime TimestampUtc { get; }

    string ContentType { get; }

    long? ContentLength { get; }

    /// <summary>
    /// The frame's own typed shape, when it knows it — a camera that produces a 1024x768 8-bit image can
    /// say so, instead of every consumer receiving an undifferentiated run of bytes.
    ///
    /// <para>Null by default, which means "a flat blob of <see cref="ContentLength"/> bytes"; a frame that
    /// has travelled through the shared-memory arena carries its descriptor there instead. This exists
    /// because the alternative — inferring rank and dtype from a content type, or from the byte count — is
    /// guesswork, and a wrong guess produces a consumer that reshapes real pixels into nonsense.</para>
    /// </summary>
    PayloadDescriptor? Descriptor => null;

    ValueTask<Stream> OpenReadAsync(CancellationToken cancellationToken);
}
