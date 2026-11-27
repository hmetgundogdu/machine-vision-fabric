using System.Text.Json.Nodes;

namespace Mvf.Worker;

/// <summary>What a payload is (descriptor field, must match the engine's PayloadDescriptor).</summary>
public enum MediaType : ushort
{
    Blob = 0,
    Tensor = 1,
    Image = 2,
    Json = 3,
}

/// <summary>The element type of a payload.</summary>
public enum ElementType : byte
{
    UInt8 = 0,
    Int8 = 1,
    UInt16 = 2,
    Int16 = 3,
    UInt32 = 4,
    Int32 = 5,
    UInt64 = 6,
    Int64 = 7,
    Float16 = 8,
    BFloat16 = 9,
    Float32 = 10,
    Float64 = 11,
}

/// <summary>
/// A typed payload read in place from the shared-memory arena (zero copy). <see cref="Data"/> points
/// into the mapped arena and is valid only during the callback that received it.
/// </summary>
public sealed unsafe class Payload
{
    internal byte* Pointer;
    internal int Length;

    internal Payload()
    {
    }

    /// <summary>The media type.</summary>
    public MediaType MediaType { get; internal set; }

    /// <summary>The element type.</summary>
    public ElementType ElementType { get; internal set; }

    /// <summary>The shape (row-major).</summary>
    public long[] Shape { get; internal set; } = [];

    /// <summary>The payload bytes, in the arena.</summary>
    public ReadOnlySpan<byte> Data => new(Pointer, Length);
}

/// <summary>A typed payload a module emits; the SDK writes it into the engine's output slot.</summary>
public sealed class Output
{
    /// <summary>A typed output.</summary>
    public Output(ReadOnlyMemory<byte> data, MediaType mediaType, ElementType elementType, long[] shape)
    {
        Data = data;
        MediaType = mediaType;
        ElementType = elementType;
        Shape = shape;
    }

    /// <summary>The payload bytes.</summary>
    public ReadOnlyMemory<byte> Data { get; }

    /// <summary>The media type.</summary>
    public MediaType MediaType { get; }

    /// <summary>The element type.</summary>
    public ElementType ElementType { get; }

    /// <summary>The shape (row-major).</summary>
    public long[] Shape { get; }

    /// <summary>A raw byte blob (u8).</summary>
    public static Output Blob(ReadOnlyMemory<byte> bytes) =>
        new(bytes, MediaType.Blob, ElementType.UInt8, [bytes.Length]);

    /// <summary>A typed tensor.</summary>
    public static Output Tensor(ReadOnlyMemory<byte> bytes, ElementType elementType, long[] shape,
        MediaType mediaType = MediaType.Tensor) => new(bytes, mediaType, elementType, shape);
}

/// <summary>The result of a classifier: a label plus an optional measurement, unit and details.</summary>
public sealed record Classification(string Label, double? Measurement = null, string? Unit = null,
    string? Details = null);

/// <summary>The result of an analyzer: any combination of a derived frame, a classification and JSON.</summary>
public sealed record AnalysisResult(Output? Frame = null, Classification? Classification = null,
    JsonNode? Value = null);

/// <summary>A frame's metadata. The SDK reuses one instance; copy what you keep.</summary>
public sealed class FrameMeta
{
    /// <summary>The source that produced the frame.</summary>
    public string CameraId { get; internal set; } = "";

    /// <summary>The frame's sequence number.</summary>
    public ulong Sequence { get; internal set; }

    /// <summary>The content type the engine reports.</summary>
    public string ContentType { get; internal set; } = "";
}

/// <summary>Optional lifecycle hooks.</summary>
public sealed class ModuleHooks
{
    /// <summary>
    /// Warmup after the handshake (load a model, connect a device). When set, the module signals
    /// readiness only after it returns.
    /// </summary>
    public Action? OnStart { get; init; }

    /// <summary>Captures durable state at a cycle boundary; null when stateless.</summary>
    public Func<Output?>? OnCheckpoint { get; init; }

    /// <summary>Rehydrates state captured earlier (usually after a restart).</summary>
    public Action<Payload>? OnRestore { get; init; }
}
