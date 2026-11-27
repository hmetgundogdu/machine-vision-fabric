namespace Mvf.Worker;

/// <summary>
/// Runs a worker module: the engine launches the executable and drives it over stdio, with payloads
/// in the shared-memory arena at MVF_ARENA_PATH. Speaks protocol v2 (binary frames) when the engine
/// offers it (MVF_PROTOCOL=2) and v1 (JSON lines) otherwise, so one build works with every host.
/// See protocol/README.md and protocol/worker-v2.md. Local only: no network.
/// </summary>
public static class WorkerModule
{
    // An offer is MVF_PROTOCOL=2 together with its launch token (MVF_SESSION): a stray
    // MVF_PROTOCOL=2 in the environment of a host that never offers v2 does not switch the module.
    private static readonly Lazy<IWire> CurrentWire = new(() =>
        Environment.GetEnvironmentVariable("MVF_PROTOCOL") == "2" &&
        !string.IsNullOrEmpty(Environment.GetEnvironmentVariable("MVF_SESSION"))
            ? new V2Wire()
            : new V1Wire());

    /// <summary>
    /// Emits a diagnostic log line to the engine: under v2 a JSON line on stderr, under v1 a
    /// <c>log</c> message. Safe from any callback: the loop is single-threaded.
    /// </summary>
    public static void Log(string message, string level = "info") => CurrentWire.Value.Log(level, message);

    /// <summary>Runs a classifier: frame in, classification out.</summary>
    public static int RunClassifier(string moduleId, Func<Payload, FrameMeta, Classification> classify,
        ModuleHooks? hooks = null)
    {
        hooks ??= new ModuleHooks();
        var writable = hooks.OnCheckpoint is not null || hooks.OnRestore is not null;
        return Serve(moduleId, "classifier", writable, hooks, (request, input, _, reply) =>
            reply.Classification = classify(input, request.Meta));
    }

    /// <summary>Runs a processor (transformer): frame in, a new frame out, or null to drop it.</summary>
    public static int RunProcessor(string moduleId, Func<Payload, FrameMeta, Output?> transform,
        ModuleHooks? hooks = null) =>
        Serve(moduleId, "processor", true, hooks ?? new ModuleHooks(), (request, input, arena, reply) =>
        {
            if (transform(input, request.Meta) is { } output)
            {
                WriteFrame(arena, request, output, reply);
            }
            else
            {
                reply.Frame = FrameOutcome.Dropped;
            }
        });

    /// <summary>Runs an analyzer: frame in, any of a derived frame, a classification and JSON out.</summary>
    public static int RunAnalyzer(string moduleId, Func<Payload, FrameMeta, AnalysisResult> analyze,
        ModuleHooks? hooks = null) =>
        Serve(moduleId, "analyzer", true, hooks ?? new ModuleHooks(), (request, input, arena, reply) =>
        {
            var result = analyze(input, request.Meta);
            reply.Frame = FrameOutcome.Dropped;
            if (result.Frame is { } frame)
            {
                WriteFrame(arena, request, frame, reply);
            }

            reply.Classification = result.Classification;
            reply.Value = result.Value;
        });

    private static void WriteFrame(Arena arena, Request request, Output output, Reply reply)
    {
        if (request.Out < 0)
        {
            throw new InvalidOperationException("execute is missing its output slot");
        }

        arena.Write(request.Out, output, request.Capacity);
        reply.Frame = FrameOutcome.Written;
        reply.Offset = request.Out;
    }

    private static int Serve(string moduleId, string capability, bool writable, ModuleHooks hooks,
        Action<Request, Payload, Arena, Reply> execute)
    {
        var path = Environment.GetEnvironmentVariable("MVF_ARENA_PATH");
        if (string.IsNullOrEmpty(path))
        {
            Console.Error.WriteLine("MVF_ARENA_PATH is not set — the shared-memory data plane is required.");
            return 1;
        }

        using var arena = new Arena(path, writable);
        var wire = CurrentWire.Value;
        if (hooks.OnStart is { } start)
        {
            wire.Hello(moduleId, capability, ready: false);
            start();
            wire.Ready(moduleId);
        }
        else
        {
            wire.Hello(moduleId, capability, ready: true);
        }

        var request = new Request();
        var reply = new Reply();
        var payload = new Payload();
        try
        {
            while (wire.Next(request))
            {
                if (request.Type == RequestType.Shutdown)
                {
                    break;
                }

                try
                {
                    switch (request.Type)
                    {
                        case RequestType.Execute:
                            reply.Reset();
                            arena.Read(request.Input, payload);
                            execute(request, payload, arena, reply);
                            wire.Result(request, reply);
                            break;
                        case RequestType.Checkpoint:
                            if (hooks.OnCheckpoint?.Invoke() is { } state)
                            {
                                if (request.Out < 0)
                                {
                                    throw new InvalidOperationException("checkpoint is missing its state slot");
                                }

                                arena.Write(request.Out, state, request.Capacity);
                                wire.State(request, empty: false);
                            }
                            else
                            {
                                wire.State(request, empty: true);
                            }

                            break;
                        case RequestType.Restore:
                            arena.Read(request.Input, payload);
                            hooks.OnRestore?.Invoke(payload);
                            wire.Restored(request);
                            break;
                        case RequestType.Invalid:
                            throw new InvalidOperationException(request.Problem);
                    }
                }
                catch (Exception e) when (e is not ProtocolException)
                {
                    wire.Error(request, e.Message);
                }
            }
        }
        catch (ProtocolException e)
        {
            Console.Error.WriteLine(e.Message);
            return 1;
        }

        return 0;
    }
}
