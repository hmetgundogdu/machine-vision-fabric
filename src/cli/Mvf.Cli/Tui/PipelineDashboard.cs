using Mvf.Graph.Execution;
using Mvf.Graph.Pipelines;
using Mvf.Abstractions;
using Spectre.Console;
using Spectre.Console.Rendering;

namespace Mvf.Cli.Tui;

/// <summary>
/// Full-screen pipeline TUI dashboard.
///
/// Rendering strategy:
///   1. Hide cursor
///   2. Before every frame: SetCursorPosition(0,0)
///   3. Write the layout via AnsiConsole (overwrites previous content)
///   4. Blank remaining lines so old content below doesn't bleed through
///   5. Restore cursor on exit
///
/// This avoids escape-sequence portability issues on Windows Console hosts
/// while still producing flicker-free in-place refresh.
/// </summary>
public sealed class PipelineDashboard
{
    private const int LogLines  = 10;
    private const int RefreshMs = 120;

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
    /// Starts the pipeline via <paramref name="options"/> and renders the live dashboard.
    /// Returns the final execution report when the run finishes.
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

        // Hide cursor to prevent flicker
        try { Console.CursorVisible = false; } catch { /* not supported on all hosts */ }
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

            RenderFrame(); // final frame
        }
        catch (OperationCanceledException) { /* clean shutdown */ }
        finally
        {
            try { Console.CursorVisible = true; } catch { }
        }

        var report = await _host.WaitForCompletionAsync(cancellationToken);
        _state.OnFinished(report?.Succeeded == false ? report.ErrorMessage : null);

        // Print final summary (cursor is at bottom after RenderFrame)
        Console.Clear();
        AnsiConsole.Write(BuildLayout(ComputeActiveLayer()));
        return report;
    }

    // ── Rendering ───────────────────────────────────────────────────────────

    private void RenderFrame()
    {
        // Jump to top-left without clearing (avoids flash)
        try { Console.SetCursorPosition(0, 0); } catch { }

        AnsiConsole.Write(BuildLayout(ComputeActiveLayer()));

        // Blank lines from current cursor position to bottom so old content doesn't bleed
        try
        {
            var cur  = Console.CursorTop;
            var rows = Console.WindowHeight > 0 ? Console.WindowHeight : 40;
            var cols = Console.WindowWidth  > 0 ? Console.WindowWidth  : 120;
            var blank = new string(' ', cols);
            for (var r = cur; r < rows - 1; r++)
            {
                Console.SetCursorPosition(0, r);
                Console.Write(blank);
            }
        }
        catch { /* ignore on non-interactive hosts */ }
    }

    // ── Layout ───────────────────────────────────────────────────────────────

    private IRenderable BuildLayout(int activeLayer)
    {
        var width    = Console.WindowWidth > 0 ? Console.WindowWidth : 120;
        var snapshot = _host.GetSnapshot();

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
            PipelineExecutionStatus.Running  => "[yellow]>> RUNNING[/]",
            PipelineExecutionStatus.Stopped  => "[green]OK DONE[/]",
            PipelineExecutionStatus.Faulted  => "[red]!! FAULTED[/]",
            PipelineExecutionStatus.Starting => "[grey].. Starting[/]",
            PipelineExecutionStatus.Stopping => "[yellow].. Stopping[/]",
            _                                => "[grey]-- Idle[/]"
        };

        var elapsed = snapshot.Elapsed.TotalSeconds > 0
            ? $"{snapshot.Elapsed.TotalSeconds:F1}s"
            : "-";

        // Truncate run ID to 8 chars
        var runId = snapshot.RunId is { Length: > 8 } r ? r[..8] : snapshot.RunId ?? "-";

        return new Markup(
            $"[bold deepskyblue1]MVF[/] [grey]|[/] " +
            $"[bold]{Markup.Escape(_definition.Name)}[/]  " +
            $"[grey]run:[/][grey58]{Markup.Escape(runId)}[/]  " +
            $"{status}  " +
            $"[grey]cyc:[/][white]{snapshot.TotalCycles}[/]  " +
            $"[grey]ok:[/][green]{snapshot.AcceptedCycles}[/]  " +
            $"[grey]t:[/][grey58]{elapsed}[/]");
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


