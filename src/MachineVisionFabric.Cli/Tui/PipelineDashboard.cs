using MachineVisionFabric.Contracts.Execution;
using MachineVisionFabric.Contracts.Pipelines;
using MachineVisionFabric.Core.Abstractions;
using Spectre.Console;
using Spectre.Console.Rendering;

namespace MachineVisionFabric.Cli.Tui;

/// <summary>
/// Full-screen pipeline TUI dashboard.
///
/// Uses the terminal alternate screen buffer so the main scroll history is
/// never disturbed and cursor drift cannot occur:
///
///   ESC[?1049h  →  enter alternate screen
///   ESC[?25l    →  hide cursor
///   ESC[H       →  move to (0,0) before each frame
///   ESC[J       →  erase from cursor to end of screen
///   ESC[?25h    →  show cursor
///   ESC[?1049l  →  leave alternate screen
///
/// Screen layout:
/// ┌─────────────────────────────────────────────────────┐
/// │  Header: run id · pipeline name · status · elapsed  │
/// ├─────────────────────────────────────────────────────┤
/// │  Graph: topological diagram, node status + stats    │
/// ├─────────────────────────────────────────────────────┤
/// │  Logs: last N lines (timestamp + level)             │
/// └─────────────────────────────────────────────────────┘
/// </summary>
public sealed class PipelineDashboard
{
    private const int LogLines  = 10;
    private const int RefreshMs = 120;

    // ANSI escape sequences (written to stdout directly)
    private const string AltScreenOn   = "\x1b[?1049h";
    private const string AltScreenOff  = "\x1b[?1049l";
    private const string CursorHide    = "\x1b[?25l";
    private const string CursorShow    = "\x1b[?25h";
    private const string CursorHome    = "\x1b[H";
    private const string EraseToEnd    = "\x1b[J";

    private readonly IPipelineExecutionHost _host;
    private readonly PipelineDefinition     _definition;
    private readonly GraphLayout            _layout;
    private readonly PipelineRenderState    _state;

    public PipelineDashboard(
        IPipelineExecutionHost host,
        PipelineDefinition definition)
    {
        _host       = host;
        _definition = definition;
        _layout     = GraphLayout.Build(definition);
        _state      = new PipelineRenderState(definition);
    }

    /// <summary>
    /// Starts the pipeline via <paramref name="options"/> and renders the live dashboard
    /// on the alternate screen buffer.  Returns the final execution report.
    /// </summary>
    public async Task<PipelineExecutionReport?> RunAsync(
        PipelineExecutionOptions options,
        CancellationToken cancellationToken = default)
    {
        var enriched = new PipelineExecutionOptions
        {
            PackageRoot      = options.PackageRoot,
            IntegrationsRoot = options.IntegrationsRoot,
            MaxCycles        = options.MaxCycles,
            OnNodeExecuted   = e => _state.OnNodeExecuted(e),
            OnCycleCompleted = p => _state.OnCycleCompleted(p)
        };

        _state.OnRunStarted(Guid.NewGuid().ToString("N")[..8], _definition.Name);
        await _host.StartAsync(_definition, enriched, cancellationToken);

        var ansi = AnsiConsole.Profile.Capabilities.Ansi;

        if (ansi)
        {
            Console.Write(AltScreenOn);
            Console.Write(CursorHide);
        }

        Console.Clear();

        try
        {
            while (!cancellationToken.IsCancellationRequested)
            {
                RenderFrame();

                var snapshot = _host.GetSnapshot();
                if (_state.IsFinished ||
                    snapshot.Status is PipelineExecutionStatus.Stopped
                                    or PipelineExecutionStatus.Faulted)
                    break;

                await Task.Delay(RefreshMs, cancellationToken).ConfigureAwait(false);
            }

            RenderFrame(); // final frame before leaving alternate screen
            await Task.Delay(150).ConfigureAwait(false);
        }
        catch (OperationCanceledException) { /* clean shutdown */ }
        finally
        {
            if (ansi)
            {
                Console.Write(CursorShow);
                Console.Write(AltScreenOff);
            }
        }

        // Execution may still be finishing — wait for the report
        var report = await _host.WaitForCompletionAsync(cancellationToken);
        _state.OnFinished(report?.Succeeded == false ? report.ErrorMessage : null);

        // Print final summary on the main screen (visible after alternate screen closes)
        AnsiConsole.Write(BuildLayout(ComputeActiveLayer(), fullScreen: false));
        return report;
    }

    // ── Rendering ───────────────────────────────────────────────────────────

    private void RenderFrame()
    {
        // Move to top-left, erase from cursor to end — no clear flash
        Console.Write(CursorHome);
        Console.Write(EraseToEnd);
        AnsiConsole.Write(BuildLayout(ComputeActiveLayer(), fullScreen: true));
    }

    private IRenderable BuildLayout(int activeLayer, bool fullScreen)
    {
        var width    = Console.WindowWidth  > 0 ? Console.WindowWidth  : 120;
        var height   = Console.WindowHeight > 0 ? Console.WindowHeight : 40;
        var snapshot = _host.GetSnapshot();

        // Reserve lines: header(1) + rule(1) + graph + rule(1) + logs(LogLines)
        var graphHeight = fullScreen
            ? Math.Max(6, height - 3 - LogLines - 2)  // -2 for rules
            : 0; // not used in static mode

        return new Rows(
            BuildHeader(snapshot),
            new Rule { Style = Style.Parse("grey23") },
            GraphRenderer.Render(_layout, _state.Nodes, width, activeLayer),
            new Rule { Style = Style.Parse("grey23") },
            BuildLogPanel()
        );
    }

    private IRenderable BuildHeader(PipelineExecutionSnapshot snapshot)
    {
        var status = snapshot.Status switch
        {
            PipelineExecutionStatus.Running  => "[yellow]⟳ RUNNING[/]",
            PipelineExecutionStatus.Stopped  => "[green]✓ DONE[/]",
            PipelineExecutionStatus.Faulted  => "[red]✖ FAULTED[/]",
            PipelineExecutionStatus.Starting => "[grey]◌ Starting…[/]",
            PipelineExecutionStatus.Stopping => "[yellow]◌ Stopping…[/]",
            _                                => "[grey]○ Idle[/]"
        };

        var elapsed = snapshot.Elapsed.TotalSeconds > 0
            ? $"{snapshot.Elapsed.TotalSeconds:F1}s"
            : "–";

        return new Markup(
            $"[bold deepskyblue1]MachineVisionFabric[/] [grey]|[/] " +
            $"[bold]{Markup.Escape(_definition.Name)}[/]  " +
            $"[grey]run:[/][grey58]{Markup.Escape(snapshot.RunId ?? "–")}[/]  " +
            $"{status}  " +
            $"[grey]cycles:[/][white]{snapshot.TotalCycles}[/]  " +
            $"[grey]accepted:[/][green]{snapshot.AcceptedCycles}[/]  " +
            $"[grey]elapsed:[/][grey58]{elapsed}[/]");
    }

    private IRenderable BuildLogPanel()
    {
        var logs = _state.GetLogs(LogLines);
        if (logs.Count == 0)
            return new Markup("[grey]  (no events yet…)[/]");

        var lines = logs.Select(l =>
        {
            var color = l.Level switch
            {
                LogLevel.Success => "green",
                LogLevel.Warning => "yellow",
                LogLevel.Error   => "red",
                _                => "grey54"
            };
            var ts = l.Timestamp.ToString("HH:mm:ss.ff");
            return $"[grey42]{ts}[/]  [{color}]{Markup.Escape(l.Message)}[/]";
        });

        return new Markup(string.Join("\n", lines));
    }

    private int ComputeActiveLayer()
    {
        var active = _state.Nodes.Values
            .Where(n => n.TotalCycles > 0)
            .OrderByDescending(n => n.TotalCycles)
            .FirstOrDefault();

        if (active is null) return 0;
        return _layout.NodePositions.TryGetValue(active.NodeId, out var pos) ? pos.Layer : 0;
    }
}

