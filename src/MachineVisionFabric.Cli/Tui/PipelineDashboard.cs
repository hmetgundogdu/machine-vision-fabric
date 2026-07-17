using MachineVisionFabric.Contracts.Execution;
using MachineVisionFabric.Contracts.Pipelines;
using MachineVisionFabric.Core.Abstractions;
using Spectre.Console;
using Spectre.Console.Rendering;

namespace MachineVisionFabric.Cli.Tui;

/// <summary>
/// Orchestrates the live pipeline TUI dashboard.
///
/// Screen layout:
/// ┌─────────────────────────────────────────────────────┐
/// │  Header: run id, pipeline name, status, elapsed     │
/// ├─────────────────────────────────────────────────────┤
/// │  Graph: topological diagram, node status + stats    │
/// ├─────────────────────────────────────────────────────┤
/// │  Logs: last N lines (scrolling, timestamp + level)  │
/// └─────────────────────────────────────────────────────┘
/// </summary>
public sealed class PipelineDashboard
{
    private const int LogLines = 10;
    private const int RefreshMs = 120;

    private readonly IPipelineExecutionHost _host;
    private readonly PipelineDefinition _definition;
    private readonly GraphLayout _layout;
    private readonly PipelineRenderState _state;

    public PipelineDashboard(
        IPipelineExecutionHost host,
        PipelineDefinition definition)
    {
        _host = host;
        _definition = definition;
        _layout = GraphLayout.Build(definition);
        _state = new PipelineRenderState(definition);
    }

    /// <summary>
    /// Starts the pipeline via <paramref name="options"/> and renders the live dashboard.
    /// Returns the final <see cref="PipelineExecutionReport"/> when the run finishes.
    /// </summary>
    public async Task<PipelineExecutionReport?> RunAsync(
        PipelineExecutionOptions options,
        CancellationToken cancellationToken = default)
    {
        // Enrich options with TUI callbacks
        var enriched = new PipelineExecutionOptions
        {
            PackageRoot = options.PackageRoot,
            IntegrationsRoot = options.IntegrationsRoot,
            MaxCycles = options.MaxCycles,
            OnNodeExecuted = e => _state.OnNodeExecuted(e),
            OnCycleCompleted = p => _state.OnCycleCompleted(p)
        };

        _state.OnRunStarted(Guid.NewGuid().ToString("N")[..8], _definition.Name);

        await _host.StartAsync(_definition, enriched, cancellationToken);

        // Track active layer for viewport
        var activeLayer = 0;

        await AnsiConsole.Live(BuildLayout(activeLayer))
            .AutoClear(false)
            .Overflow(VerticalOverflow.Ellipsis)
            .Cropping(VerticalOverflowCropping.Bottom)
            .StartAsync(async ctx =>
            {
                while (!_state.IsFinished)
                {
                    // Update active layer = layer of node with most recent execution
                    activeLayer = ComputeActiveLayer();
                    ctx.UpdateTarget(BuildLayout(activeLayer));
                    ctx.Refresh();

                    var snapshot = _host.GetSnapshot();
                    if (snapshot.Status is
                        Contracts.Execution.PipelineExecutionStatus.Stopped or
                        Contracts.Execution.PipelineExecutionStatus.Faulted)
                    {
                        break;
                    }

                    await Task.Delay(RefreshMs, cancellationToken).ConfigureAwait(false);
                }

                // Final render
                activeLayer = ComputeActiveLayer();
                ctx.UpdateTarget(BuildLayout(activeLayer));
                ctx.Refresh();
            });

        var report = await _host.WaitForCompletionAsync(cancellationToken);
        _state.OnFinished(report?.Succeeded == false ? report.ErrorMessage : null);

        // Print final static summary
        AnsiConsole.Write(BuildLayout(activeLayer));
        return report;
    }

    // ── Renderable construction ─────────────────────────────────────────────

    private IRenderable BuildLayout(int activeLayer)
    {
        var width = Console.WindowWidth > 0 ? Console.WindowWidth : 120;
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
            Contracts.Execution.PipelineExecutionStatus.Running  => "[yellow]⟳ Running[/]",
            Contracts.Execution.PipelineExecutionStatus.Stopped  => "[green]✓ Stopped[/]",
            Contracts.Execution.PipelineExecutionStatus.Faulted  => "[red]✖ Faulted[/]",
            Contracts.Execution.PipelineExecutionStatus.Starting => "[grey]◌ Starting[/]",
            Contracts.Execution.PipelineExecutionStatus.Stopping => "[yellow]◌ Stopping[/]",
            _                                                     => "[grey]○ Idle[/]"
        };

        var elapsed = snapshot.Elapsed.TotalSeconds > 0
            ? $"{snapshot.Elapsed.TotalSeconds:F1}s"
            : "–";

        return new Markup(
            $"[bold]{Markup.Escape(_definition.Name)}[/]  " +
            $"[grey]run:[/][grey62]{Markup.Escape(snapshot.RunId ?? "–")}[/]  " +
            $"{status}  " +
            $"[grey]cycles:[/][grey62]{snapshot.TotalCycles}[/]  " +
            $"[grey]accepted:[/][green]{snapshot.AcceptedCycles}[/]  " +
            $"[grey]elapsed:[/][grey62]{elapsed}[/]");
    }

    private IRenderable BuildLogPanel()
    {
        var logs = _state.GetLogs(LogLines);
        if (logs.Count == 0)
            return new Markup("[grey]  (no logs yet)[/]");

        var lines = logs.Select(l =>
        {
            var color = l.Level switch
            {
                LogLevel.Success => "green",
                LogLevel.Warning => "yellow",
                LogLevel.Error   => "red",
                _                => "grey62"
            };
            var ts = l.Timestamp.ToString("HH:mm:ss.ff");
            return $"[grey]{ts}[/]  [{color}]{Markup.Escape(l.Message)}[/]";
        });

        return new Markup(string.Join("\n", lines));
    }

    private int ComputeActiveLayer()
    {
        // Find the layer of the node that was last active/executed
        var active = _state.Nodes.Values
            .Where(n => n.TotalCycles > 0)
            .OrderByDescending(n => n.TotalCycles)
            .FirstOrDefault();

        if (active is null) return 0;
        return _layout.NodePositions.TryGetValue(active.NodeId, out var pos) ? pos.Layer : 0;
    }
}
