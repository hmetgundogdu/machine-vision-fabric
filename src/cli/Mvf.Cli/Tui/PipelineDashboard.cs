using System.Text.Json.Nodes;
using Mvf.Graph.Execution;
using Mvf.Graph.Pipelines;
using Mvf.Graph.Values;
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
    private readonly LiveValueRegistry?     _liveValues;

    /// <summary>
    /// Whether the graph carries a <c>loop</c> — only then is there a running state to pause. Space toggles
    /// it, and the header shows PAUSED; a graph with no loop has no pause control at all.
    /// </summary>
    private readonly bool _hasLoop;

    /// <summary>Index into the tunables list; -1 until the run registers any.</summary>
    private int _selectedTunable = -1;

    /// <summary>Last edit outcome, shown under the panel until the next one.</summary>
    private string? _editNotice;

    public PipelineDashboard(
        IPipelineExecutionHost host,
        PipelineDefinition definition,
        LiveValueRegistry? liveValues = null)
    {
        _host       = host;
        _definition = definition;
        _layout     = GraphLayout.Build(definition);
        _state      = new PipelineRenderState(definition);
        _liveValues = liveValues;
        _hasLoop    = definition.Nodes.Any(n =>
            string.Equals(n.Kind, "embedded-primitive", StringComparison.OrdinalIgnoreCase)
            && string.Equals(n.PrimitiveType, "loop", StringComparison.OrdinalIgnoreCase));
    }

    private bool IsPaused => _hasLoop && _liveValues?.RunControl.IsPaused == true;

    /// <summary>
    /// Starts the pipeline via <paramref name="options"/> and renders the live dashboard.
    /// Returns the final execution report when the run finishes.
    /// </summary>
    public async Task<PipelineExecutionReport?> RunAsync(
        PipelineExecutionOptions options,
        CancellationToken cancellationToken = default)
    {
        // The dashboard only adds observation callbacks; `with` carries every other run option through.
        var enriched = options with
        {
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

                // Keys are drained between frames rather than on a reader thread: an edit has to own the
                // screen for as long as it takes, and the run keeps going the whole time — the executor
                // is a separate task and never waits on this loop.
                HandleKeys();

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

    // ── Live tunables ────────────────────────────────────────────────────────

    private IReadOnlyList<LiveValue> Tunables => _liveValues?.Values ?? [];

    private void HandleKeys()
    {
        bool available;
        try { available = Console.KeyAvailable; }
        catch { return; }   // redirected input — no keyboard to read

        while (available)
        {
            var key = Console.ReadKey(intercept: true);
            var tunables = Tunables;

            // Space pauses/resumes the whole run. It is handled ahead of — and independently of — the
            // tunables, because a loop-carrying graph may have no value nodes at all yet still be pausable.
            // Pause is not cancel: the loop stops advancing, the run keeps its state and its warm workers.
            if (key.Key == ConsoleKey.Spacebar && _hasLoop && _liveValues is not null)
            {
                _liveValues.RunControl.Toggle();
            }
            else if (tunables.Count > 0)
            {
                switch (key.Key)
                {
                    case ConsoleKey.UpArrow:
                        _selectedTunable = (_selectedTunable <= 0 ? tunables.Count : _selectedTunable) - 1;
                        break;

                    case ConsoleKey.DownArrow:
                    case ConsoleKey.Tab:
                        _selectedTunable = (_selectedTunable + 1) % tunables.Count;
                        break;

                    case ConsoleKey.Enter:
                        if (_selectedTunable < 0) _selectedTunable = 0;
                        EditSelected(tunables[_selectedTunable]);
                        break;
                }
            }

            try { available = Console.KeyAvailable; }
            catch { return; }
        }
    }

    /// <summary>
    /// Takes over the screen, asks for a new setting, and hands it to the registry — which type- and
    /// schema-checks it exactly as a literal or a stored binding is checked. The pipeline keeps running
    /// throughout; the node picks the change up on its next cycle.
    /// </summary>
    private void EditSelected(LiveValue tunable)
    {
        Console.Clear();
        try { Console.CursorVisible = true; } catch { }

        try
        {
            if (tunable.Choices is { Count: > 0 } choices)
            {
                PickFrom(tunable, choices);
                return;
            }

            var typeHint = tunable.Shape == ControlValueShape.List
                ? $"list of {ControlValueTypes.ToToken(tunable.Type)} — write it as JSON"
                : ControlValueTypes.ToToken(tunable.Type);
            var currentText = tunable.Current?.ToJsonString() ?? string.Empty;

            AnsiConsole.MarkupLine($"[bold]{Markup.Escape(tunable.Label)}[/]  [grey]({Markup.Escape(tunable.NodeId)}, {Markup.Escape(typeHint)})[/]");
            AnsiConsole.MarkupLine($"[grey]current:[/] {Markup.Escape(currentText)}");
            AnsiConsole.MarkupLine(tunable.Binding is { Length: > 0 } binding
                ? $"[grey]Saved to binding[/] [grey58]{Markup.Escape(binding)}[/]"
                : "[grey]No binding — this change lasts for the run only.[/]");
            AnsiConsole.WriteLine();

            // The current value is printed above rather than handed to DefaultValue: Spectre renders a
            // shown default as markup, and a JSON value's '[' would be read as a style tag and throw.
            // Empty input therefore means "leave it alone", which is also the safer default mid-run.
            var answer = AnsiConsole.Prompt(
                new TextPrompt<string>("[green]>[/] new value [grey](enter to keep)[/]:")
                    .AllowEmpty());

            if (string.IsNullOrWhiteSpace(answer))
            {
                _editNotice = null;
                return;
            }

            if (!ControlValueTypes.TryParseShaped(tunable.Shape, tunable.Type, answer.Trim(), out var parsed))
            {
                _editNotice = $"[red]{Markup.Escape(tunable.NodeId)}: '{Markup.Escape(answer.Trim())}' is not a valid {Markup.Escape(typeHint)}[/]";
                return;
            }

            _editNotice = _liveValues!.TrySet(tunable.NodeId, parsed, out var error)
                ? $"[green]{Markup.Escape(tunable.NodeId)} = {Markup.Escape(parsed?.ToJsonString() ?? "null")}[/]"
                : $"[red]{Markup.Escape(tunable.NodeId)}: {Markup.Escape(error ?? "rejected")}[/]";
        }
        catch (Exception ex) when (ex is IOException or InvalidOperationException)
        {
            _editNotice = "[red]could not read from the terminal[/]";
        }
        finally
        {
            try { Console.CursorVisible = false; } catch { }
            Console.Clear();
        }
    }

    /// <summary>
    /// Re-choosing mid-run, from the collection the node is narrowing <i>right now</i> — the runner
    /// publishes it every cycle, so this is the live candidate list, not the one the process started with.
    /// The pipeline keeps running while the list is open; the choice lands on the next cycle.
    /// </summary>
    private void PickFrom(LiveValue tunable, IReadOnlyList<JsonNode?> choices)
    {
        AnsiConsole.MarkupLine($"[bold]{Markup.Escape(tunable.Label)}[/]  [grey]({Markup.Escape(tunable.NodeId)})[/]");
        AnsiConsole.MarkupLine($"[grey]current:[/] {Markup.Escape(tunable.Current?.ToJsonString() ?? "none")}");
        AnsiConsole.MarkupLine(tunable.Binding is { Length: > 0 } binding
            ? $"[grey]Saved to binding[/] [grey58]{Markup.Escape(binding)}[/]"
            : "[grey]No binding — this change lasts for the run only.[/]");
        AnsiConsole.WriteLine();

        const string cancel = "(leave it alone)";
        var labels = choices.Select(DescribeChoice).ToList();
        labels.Add(cancel);

        var picked = AnsiConsole.Prompt(
            new SelectionPrompt<string>()
                .Title("[green]>[/] pick one")
                .AddChoices(labels));

        if (picked == cancel)
        {
            _editNotice = null;
            return;
        }

        var chosen = choices[labels.IndexOf(picked)];

        // With a `by`, store the identifying property rather than the whole record — the same rule the
        // pre-pass picker follows, so a mid-run choice and a first-run choice write the same binding.
        var value = tunable.ChoiceLabelProperty is { Length: > 0 } property
                    && chosen is JsonObject obj
                    && obj.TryGetPropertyValue(property, out var identifier)
            ? identifier?.DeepClone()
            : chosen?.DeepClone();

        _editNotice = _liveValues!.TrySet(tunable.NodeId, value, out var error)
            ? $"[green]{Markup.Escape(tunable.NodeId)} = {Markup.Escape(value?.ToJsonString() ?? "null")}[/]"
            : $"[red]{Markup.Escape(tunable.NodeId)}: {Markup.Escape(error ?? "rejected")}[/]";
    }

    private static string DescribeChoice(JsonNode? choice) => choice switch
    {
        null => "(null)",
        JsonObject obj => string.Join("  ", obj.Take(3).Select(p => $"{p.Key}={p.Value?.ToJsonString() ?? "null"}")),
        _ => choice.ToJsonString()
    };

    private IRenderable? BuildTunablePanel()
    {
        var tunables = Tunables;
        if (tunables.Count == 0)
        {
            return null;
        }

        if (_selectedTunable >= tunables.Count)
        {
            _selectedTunable = tunables.Count - 1;
        }

        // Node id rather than the label: the graph above is drawn with node ids, so the eye can match the
        // two, and three of them still fit on one line where three prompts would not.
        var cells = tunables.Select((t, i) =>
        {
            var value = t.Current?.ToJsonString() ?? "null";
            var pinned = t.Binding is { Length: > 0 } ? string.Empty : "[grey42]*[/]";
            return i == _selectedTunable
                ? $"[black on deepskyblue1] {Markup.Escape(t.NodeId)}={Markup.Escape(value)} [/]{pinned}"
                : $"[grey]{Markup.Escape(t.NodeId)}=[/][white]{Markup.Escape(value)}[/]{pinned}";
        });

        var picking = _selectedTunable >= 0 && tunables[_selectedTunable].Choices is { Count: > 0 };
        var hint = _selectedTunable < 0
            ? "[grey42]tab/↑↓ pick · enter edit[/]"
            : picking ? "[grey42]enter choose[/]" : "[grey42]enter edit[/]";

        var line = $"[grey]tune[/]  {string.Join("   ", cells)}   {hint}";

        return _editNotice is null
            ? new Markup(line)
            : new Rows(new Markup(line), new Markup($"      {_editNotice}"));
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

        var rows = new List<IRenderable>
        {
            BuildHeader(snapshot),
            new Rule { Style = Style.Parse("grey23") },
            GraphRenderer.Render(_layout, _state.Nodes, width, activeLayer)
        };

        if (BuildTunablePanel() is { } tunables)
        {
            rows.Add(new Rule { Style = Style.Parse("grey23") });
            rows.Add(tunables);
        }

        rows.Add(new Rule { Style = Style.Parse("grey23") });
        rows.Add(BuildLogPanel());

        return new Rows(rows);
    }

    private IRenderable BuildHeader(PipelineExecutionSnapshot snapshot)
    {
        // Paused overrides the running readout: the run is alive and idling, not stopped, so the plain
        // "RUNNING" would misread. Only meaningful while the run is actually going.
        var status = IsPaused && snapshot.Status == PipelineExecutionStatus.Running
            ? "[black on gold1] || PAUSED [/]"
            : snapshot.Status switch
            {
                PipelineExecutionStatus.Running  => "[yellow]>> RUNNING[/]",
                PipelineExecutionStatus.Stopped  => "[green]OK DONE[/]",
                PipelineExecutionStatus.Faulted  => "[red]!! FAULTED[/]",
                PipelineExecutionStatus.Starting => "[grey].. Starting[/]",
                PipelineExecutionStatus.Stopping => "[yellow].. Stopping[/]",
                _                                => "[grey]-- Idle[/]"
            };

        // Surface the shortcut only where it does something — a graph with a loop.
        var pauseHint = _hasLoop && snapshot.Status == PipelineExecutionStatus.Running
            ? "  [grey42]space:" + (IsPaused ? "resume" : "pause") + "[/]"
            : string.Empty;

        var elapsed = snapshot.Elapsed.TotalSeconds > 0
            ? $"{snapshot.Elapsed.TotalSeconds:F1}s"
            : "-";

        // Truncate run ID to 8 chars
        var runId = snapshot.RunId is { Length: > 8 } r ? r[..8] : snapshot.RunId ?? "-";

        // Cross-process recovery, promoted to the header: a restart is transparent to the graph, so
        // without this a run that lost and replaced a worker looks identical to one that never did.
        var restarts = _state.GetNodeSnapshot().Sum(n => n.WorkerRestarts);
        var restartCell = restarts > 0
            ? $"  [grey]rst:[/][red]{restarts}[/]"
            : string.Empty;

        return new Markup(
            $"[bold deepskyblue1]MVF[/] [grey]|[/] " +
            $"[bold]{Markup.Escape(_definition.Name)}[/]  " +
            $"[grey]run:[/][grey58]{Markup.Escape(runId)}[/]  " +
            $"{status}  " +
            $"[grey]cyc:[/][white]{snapshot.TotalCycles}[/]  " +
            $"[grey]ok:[/][green]{snapshot.AcceptedCycles}[/]  " +
            $"[grey]t:[/][grey58]{elapsed}[/]" +
            restartCell +
            pauseHint);
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


