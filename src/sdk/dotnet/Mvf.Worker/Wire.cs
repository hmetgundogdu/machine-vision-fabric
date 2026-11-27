using System.Buffers;
using System.Buffers.Binary;
using System.Text;
using System.Text.Json;
using System.Text.Json.Nodes;

namespace Mvf.Worker;

internal enum RequestType
{
    Execute,
    Checkpoint,
    Restore,
    Shutdown,
    Invalid,
}

/// <summary>A request from the engine, whichever protocol carried it.</summary>
internal sealed class Request
{
    public RequestType Type;
    public JsonNode? Id; // v1: the id as sent, echoed back
    public ulong Rid; // v2: the correlation id
    public long Input = -1; // execute: the input slot; restore: the state slot
    public long Out = -1; // execute/checkpoint: the output slot
    public long Capacity;
    public readonly FrameMeta Meta = new();
    public string Problem = "";
}

internal enum FrameOutcome
{
    Absent,
    Dropped,
    Written,
}

/// <summary>A result, whichever protocol carries it.</summary>
internal sealed class Reply
{
    public FrameOutcome Frame;
    public long Offset;
    public Classification? Classification;
    public JsonNode? Value;

    public void Reset()
    {
        Frame = FrameOutcome.Absent;
        Offset = 0;
        Classification = null;
        Value = null;
    }
}

/// <summary>The engine broke the protocol: the module stops; it never resynchronises.</summary>
internal sealed class ProtocolException(string message) : Exception(message);

internal interface IWire
{
    void Hello(string moduleId, string capability, bool ready);
    void Ready(string moduleId);
    bool Next(Request request);
    void Result(Request request, Reply reply);
    void Error(Request request, string message);
    void State(Request request, bool empty);
    void Restored(Request request);
    void Log(string level, string message);
}

/// <summary>Protocol v1: one JSON object per line on stdin/stdout.</summary>
internal sealed class V1Wire : IWire
{
    private readonly StreamReader _in = new(Console.OpenStandardInput(), new UTF8Encoding(false));
    private readonly StreamWriter _out = new(Console.OpenStandardOutput(), new UTF8Encoding(false));

    public void Hello(string moduleId, string capability, bool ready)
    {
        var hello = new JsonObject
        {
            ["type"] = "hello",
            ["protocol"] = 1,
            ["moduleId"] = moduleId,
            ["capability"] = capability,
        };
        if (!ready)
        {
            hello["ready"] = false;
        }

        Send(hello);
    }

    public void Ready(string moduleId) => Send(new JsonObject { ["type"] = "ready", ["moduleId"] = moduleId });

    public bool Next(Request request)
    {
        while (_in.ReadLine() is { } line)
        {
            if (line.Length == 0)
            {
                continue;
            }

            JsonObject? msg;
            try
            {
                msg = JsonNode.Parse(line) as JsonObject;
            }
            catch (JsonException)
            {
                continue;
            }

            if (msg is null)
            {
                continue;
            }

            var type = msg["type"] is JsonValue t && t.TryGetValue<string>(out var s) ? s : "";
            request.Id = msg["id"]?.DeepClone();
            request.Input = -1;
            request.Out = -1;
            request.Capacity = 0;
            try
            {
                switch (type)
                {
                    case "shutdown":
                        request.Type = RequestType.Shutdown;
                        break;
                    case "execute":
                        request.Type = RequestType.Execute;
                        var frame = msg["frame"] as JsonObject;
                        request.Meta.CameraId = (string?)frame?["cameraId"] ?? "";
                        request.Meta.Sequence = (ulong?)frame?["sequence"] ?? 0;
                        request.Meta.ContentType = (string?)frame?["contentType"] ?? "";
                        request.Input = (long?)frame?["shm"]?["offset"] ?? -1;
                        Slot(msg, request);
                        break;
                    case "checkpoint":
                        request.Type = RequestType.Checkpoint;
                        Slot(msg, request);
                        break;
                    case "restore":
                        request.Type = RequestType.Restore;
                        request.Input = (long?)msg["shm"]?["offset"] ?? -1;
                        break;
                    default:
                        continue; // a message this module does not handle
                }
            }
            catch (Exception e) when (e is InvalidOperationException or FormatException)
            {
                request.Type = RequestType.Invalid;
                request.Problem = e.Message;
            }

            return true;
        }

        return false;
    }

    public void Result(Request request, Reply reply)
    {
        var result = new JsonObject { ["type"] = "result", ["id"] = request.Id?.DeepClone() };
        if (reply.Frame == FrameOutcome.Dropped)
        {
            result["frame"] = null;
        }
        else if (reply.Frame == FrameOutcome.Written)
        {
            result["frame"] = new JsonObject { ["shm"] = new JsonObject { ["offset"] = reply.Offset } };
        }

        if (reply.Classification is { } c)
        {
            result["classification"] = new JsonObject
            {
                ["label"] = c.Label,
                ["measurement"] = c.Measurement,
                ["unit"] = c.Unit,
                ["details"] = c.Details,
            };
        }

        if (reply.Value is { } value)
        {
            result["value"] = value.DeepClone();
        }

        Send(result);
    }

    public void Error(Request request, string message) => Send(new JsonObject
    {
        ["type"] = "error",
        ["id"] = request.Id?.DeepClone(),
        ["message"] = message,
    });

    public void State(Request request, bool empty)
    {
        var state = new JsonObject { ["type"] = "state", ["id"] = request.Id?.DeepClone() };
        if (empty)
        {
            state["empty"] = true;
        }

        Send(state);
    }

    public void Restored(Request request) =>
        Send(new JsonObject { ["type"] = "restored", ["id"] = request.Id?.DeepClone() });

    public void Log(string level, string message) =>
        Send(new JsonObject { ["type"] = "log", ["level"] = level, ["message"] = message });

    private void Send(JsonObject message)
    {
        _out.Write(message.ToJsonString());
        _out.Write('\n');
        _out.Flush();
    }

    private static void Slot(JsonObject msg, Request request)
    {
        if (msg["out"] is not JsonObject slot)
        {
            return;
        }

        request.Out = (long?)slot["offset"] ?? throw new InvalidOperationException("slot without an offset");
        request.Capacity = (long?)slot["capacity"] ?? 0;
    }
}

/// <summary>
/// Protocol v2: length-prefixed binary frames on the inherited stdio pipes (protocol/worker-v2.md).
/// Hot-path bodies are fixed binary layouts; JSON only for hello, errors and result tails. Logs go to
/// stderr as JSON lines.
/// </summary>
internal sealed class V2Wire : IWire
{
    private const int Header = 24;
    private const uint MaxBody = 1u << 20;
    private const ulong None = ulong.MaxValue;
    private const byte KHello = 1, KReady = 2, KExecute = 3, KResult = 4, KError = 5, KCheckpoint = 8,
        KState = 9, KRestore = 10, KRestored = 11, KPing = 12, KPong = 13, KShutdown = 15;

    private readonly Stream _in;
    private readonly Stream _out;
    private readonly Stream _err = Console.OpenStandardError();
    private readonly string _session;
    private uint _generation;
    private byte[] _buf = new byte[64 * 1024];
    private int _start;
    private int _end;
    private byte[] _tx = new byte[256];
    private readonly ArrayBufferWriter<byte> _json = new(256);
    private readonly Utf8JsonWriter _writer;
    private byte[] _camera = [];
    private byte[] _content = [];

    public V2Wire()
    {
        _session = Environment.GetEnvironmentVariable("MVF_SESSION") ?? "";
        _generation = uint.TryParse(Environment.GetEnvironmentVariable("MVF_GENERATION"), out var g) ? g : 1;
        (_in, _out) = Channel.Protect();
        _writer = new Utf8JsonWriter(_json);
    }

    public void Hello(string moduleId, string capability, bool ready)
    {
        var hello = new JsonObject
        {
            ["type"] = "hello",
            ["protocol"] = 2,
            ["moduleId"] = moduleId,
            ["capability"] = capability,
            ["session"] = _session,
        };
        if (!ready)
        {
            hello["ready"] = false;
        }

        Frame(KHello, 0, Encoding.UTF8.GetBytes(hello.ToJsonString()));
    }

    public void Ready(string moduleId) =>
        Frame(KReady, 0, Encoding.UTF8.GetBytes(new JsonObject { ["type"] = "ready", ["moduleId"] = moduleId }.ToJsonString()));

    public bool Next(Request request)
    {
        while (true)
        {
            if (!Need(Header))
            {
                return false;
            }

            var h = _buf.AsSpan(_start, Header);
            if (h[0] != (byte)'M' || h[1] != (byte)'V' || h[2] != 2)
            {
                throw new ProtocolException("protocol v2: bad frame from the engine");
            }

            var kind = h[3];
            var id = BinaryPrimitives.ReadUInt64LittleEndian(h[8..]);
            var generation = BinaryPrimitives.ReadUInt32LittleEndian(h[16..]);
            var length = BinaryPrimitives.ReadUInt32LittleEndian(h[20..]);
            if (length > MaxBody)
            {
                throw new ProtocolException("protocol v2: a frame from the engine exceeds 1 MiB");
            }

            if (!Need(Header + (int)length))
            {
                return false;
            }

            var body = _buf.AsSpan(_start + Header, (int)length);
            _start += Header + (int)length;
            _generation = generation;
            request.Rid = id;
            request.Input = -1;
            request.Out = -1;
            request.Capacity = 0;
            switch (kind)
            {
                case KExecute:
                    request.Type = RequestType.Execute;
                    Execute(body, request);
                    return true;
                case KCheckpoint:
                    if (length != 16)
                    {
                        throw new ProtocolException("protocol v2: malformed checkpoint");
                    }

                    request.Type = RequestType.Checkpoint;
                    request.Out = (long)BinaryPrimitives.ReadUInt64LittleEndian(body);
                    request.Capacity = (long)BinaryPrimitives.ReadUInt64LittleEndian(body[8..]);
                    return true;
                case KRestore:
                    if (length < 8)
                    {
                        throw new ProtocolException("protocol v2: malformed restore");
                    }

                    request.Type = RequestType.Restore;
                    request.Input = (long)BinaryPrimitives.ReadUInt64LittleEndian(body);
                    return true;
                case KPing:
                    Frame(KPong, id, []);
                    continue;
                case KShutdown:
                    request.Type = RequestType.Shutdown;
                    return true;
                default:
                    // cancel: this loop is single-threaded, so a cancel arrives after its request
                    // finished; the engine's deadline bounds a slow request. configure: never sent,
                    // this SDK does not advertise the feature.
                    continue;
            }
        }
    }

    public void Result(Request request, Reply reply)
    {
        uint flags = reply.Frame switch
        {
            FrameOutcome.Dropped => 2,
            FrameOutcome.Written => 1,
            _ => 0,
        };
        _json.ResetWrittenCount();
        if (reply.Classification is not null || reply.Value is not null)
        {
            _writer.Reset(_json);
            _writer.WriteStartObject();
            if (reply.Classification is { } c)
            {
                _writer.WriteStartObject("classification");
                _writer.WriteString("label", c.Label);
                WriteNullable(_writer, "measurement", c.Measurement);
                WriteNullable(_writer, "unit", c.Unit);
                WriteNullable(_writer, "details", c.Details);
                _writer.WriteEndObject();
            }

            if (reply.Value is { } value)
            {
                _writer.WritePropertyName("value");
                value.WriteTo(_writer);
            }

            _writer.WriteEndObject();
            _writer.Flush();
            flags |= 8;
        }

        var tail = _json.WrittenSpan;
        Span<byte> body = stackalloc byte[24];
        BinaryPrimitives.WriteUInt32LittleEndian(body, flags);
        BinaryPrimitives.WriteUInt32LittleEndian(body[4..], 0);
        BinaryPrimitives.WriteUInt64LittleEndian(body[8..],
            reply.Frame == FrameOutcome.Written ? (ulong)reply.Offset : 0);
        BinaryPrimitives.WriteUInt64LittleEndian(body[16..], 0);
        Frame(KResult, request.Rid, body, tail);
    }

    public void Error(Request request, string message) =>
        Frame(KError, request.Rid, Encoding.UTF8.GetBytes(new JsonObject { ["message"] = message }.ToJsonString()));

    public void State(Request request, bool empty) => Frame(KState, request.Rid, [], flags: (ushort)(empty ? 1 : 0));

    public void Restored(Request request) => Frame(KRestored, request.Rid, []);

    public void Log(string level, string message)
    {
        var line = new JsonObject { ["level"] = level, ["message"] = message }.ToJsonString() + "\n";
        _err.Write(Encoding.UTF8.GetBytes(line));
        _err.Flush();
    }

    private static void WriteNullable(Utf8JsonWriter w, string name, double? v)
    {
        if (v is { } x)
        {
            w.WriteNumber(name, x);
        }
        else
        {
            w.WriteNull(name);
        }
    }

    private static void WriteNullable(Utf8JsonWriter w, string name, string? v)
    {
        if (v is null)
        {
            w.WriteNull(name);
        }
        else
        {
            w.WriteString(name, v);
        }
    }

    private void Execute(ReadOnlySpan<byte> body, Request request)
    {
        if (body.Length < 40)
        {
            throw new ProtocolException("protocol v2: malformed execute");
        }

        var input = BinaryPrimitives.ReadUInt64LittleEndian(body);
        var sequence = BinaryPrimitives.ReadUInt64LittleEndian(body[8..]);
        var output = BinaryPrimitives.ReadUInt64LittleEndian(body[16..]);
        var capacity = BinaryPrimitives.ReadUInt64LittleEndian(body[24..]);
        int cameraLength = BinaryPrimitives.ReadUInt16LittleEndian(body[36..]);
        int contentLength = BinaryPrimitives.ReadUInt16LittleEndian(body[38..]);
        if (40 + cameraLength + contentLength > body.Length)
        {
            throw new ProtocolException("protocol v2: malformed execute");
        }

        request.Out = output == None ? -1 : (long)output;
        request.Capacity = (long)capacity;
        if (input == None)
        {
            return;
        }

        request.Input = (long)input;
        request.Meta.Sequence = sequence;
        // Strings are decoded only when they change, so a frame costs no allocation for them.
        var camera = body.Slice(40, cameraLength);
        if (!camera.SequenceEqual(_camera))
        {
            _camera = camera.ToArray();
            request.Meta.CameraId = Encoding.UTF8.GetString(camera);
        }

        var content = body.Slice(40 + cameraLength, contentLength);
        if (!content.SequenceEqual(_content))
        {
            _content = content.ToArray();
            request.Meta.ContentType = Encoding.UTF8.GetString(content);
        }
    }

    // Ensures n unread bytes are buffered; false at the end of the stream. Usually one read returns
    // a whole request, so a request costs one system call.
    private bool Need(int n)
    {
        if (_end - _start >= n)
        {
            return true;
        }

        if (_start > 0)
        {
            Buffer.BlockCopy(_buf, _start, _buf, 0, _end - _start);
            _end -= _start;
            _start = 0;
        }

        if (_buf.Length < n)
        {
            Array.Resize(ref _buf, Math.Max(n, _buf.Length * 2));
        }

        while (_end < n)
        {
            var read = _in.Read(_buf, _end, _buf.Length - _end);
            if (read <= 0)
            {
                return false;
            }

            _end += read;
        }

        return true;
    }

    private void Frame(byte kind, ulong id, ReadOnlySpan<byte> body, ReadOnlySpan<byte> tail = default,
        ushort flags = 0)
    {
        var total = Header + body.Length + tail.Length;
        if (_tx.Length < total)
        {
            _tx = new byte[Math.Max(total, _tx.Length * 2)];
        }

        var h = _tx.AsSpan(0, Header);
        h[0] = (byte)'M';
        h[1] = (byte)'V';
        h[2] = 2;
        h[3] = kind;
        BinaryPrimitives.WriteUInt16LittleEndian(h[4..], flags);
        BinaryPrimitives.WriteUInt16LittleEndian(h[6..], 0);
        BinaryPrimitives.WriteUInt64LittleEndian(h[8..], id);
        BinaryPrimitives.WriteUInt32LittleEndian(h[16..], _generation);
        BinaryPrimitives.WriteUInt32LittleEndian(h[20..], (uint)(body.Length + tail.Length));
        body.CopyTo(_tx.AsSpan(Header));
        tail.CopyTo(_tx.AsSpan(Header + body.Length));
        _out.Write(_tx, 0, total); // one write per frame
    }
}
