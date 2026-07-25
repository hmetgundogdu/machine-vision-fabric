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

    /// <summary>
    /// The operator's navigation cursor: an index into <see cref="GraphLayout.TraversalOrder"/>. Until the
    /// operator first presses an arrow key the cursor auto-follows the executing node (and the viewport with
    /// it); after that it is theirs to move, and the viewport tracks it so a selected node is never off-page.
    /// </summary>
    private int _selectedIndex;

    /// <summary>True once the operator has taken manual control of the cursor with an arrow key.</summary>
    private bool _userNavigated;

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
        AnsiConsole.Write(BuildLayout());
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

            // Space pauses/resumes the whole run, independently of the cursor: a loop-carrying graph may
            // have no value nodes at all yet still be pausable. Pause is not cancel — the loop stops
            // advancing, the run keeps its state and its warm workers.
            switch (key.Key)
            {
                case ConsoleKey.Spacebar when _hasLoop && _liveValues is not null:
                    _liveValues.RunControl.Toggle();
                    break;

                case ConsoleKey.LeftArrow:  MoveCursor(-1); break;
                case ConsoleKey.RightArrow: MoveCursor(+1); break;
                case ConsoleKey.Tab:        MoveCursor(+1); break;
                case ConsoleKey.UpArrow:    MoveCursorWithinLayer(-1); break;
                case ConsoleKey.DownArrow:  MoveCursorWithinLayer(+1); break;

                case ConsoleKey.Enter:
                    if (SelectedNodeId is { } nodeId) OpenNodeDetail(nodeId);
                    break;
            }

            try { available = Console.KeyAvailable; }
            catch { return; }
        }
    }

    // ── Cursor / navigation ───────────────────────────────────────────────────

    private IReadOnlyList<string> Order => _layout.TraversalOrder;

    /// <summary>The node under the cursor right now, or null for an empty graph.</summary>
    private string? SelectedNodeId =>
        Order.Count == 0 ? null : Order[Math.Clamp(_selectedIndex, 0, Order.Count - 1)];

    /// <summary>Steps the cursor through the flat left-to-right order. Wraps at both ends.</summary>
    private void MoveCursor(int delta)
    {
        if (Order.Count == 0) return;
        _userNavigated = true;
        _selectedIndex = (_selectedIndex + delta % Order.Count + Order.Count) % Order.Count;
    }

    /// <summary>Moves the cursor to the previous/next slot within its current layer, if one exists.</summary>
    private void MoveCursorWithinLayer(int delta)
    {
        if (SelectedNodeId is not { } current) return;
        if (!_layout.NodePositions.TryGetValue(current, out var pos)) return;

        var layer = _layout.Layers[pos.Layer];
        var target = pos.Slot + delta;
        if (target < 0 || target >= layer.Count) return;

        _userNavigated = true;
        _selectedIndex = Order.ToList().IndexOf(layer[target]);
    }

    /// <summary>
    /// Before the operator takes over, the cursor rides the executing node so both the highlight and the
    /// viewport follow the work. Called once per frame.
    /// </summary>
    private void SyncCursorToActive()
    {
        if (_userNavigated) return;
        if (_state.LastActiveNodeId is not { } active) return;

        var idx = Order.ToList().IndexOf(active);
        if (idx >= 0) _selectedIndex = idx;
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

    /// <summary>
    /// The live tunables that belong to one node: a <c>value</c>/<c>select</c> node registers under its own
    /// id, a module binding registers under <c>{nodeId}.{field}</c>. Both are folded in here so the node's
    /// config box and detail view show — and edit — everything editable about it.
    /// </summary>
    private IReadOnlyList<LiveValue> NodeTunables(string nodeId) =>
        Tunables
            .Where(t => string.Equals(t.NodeId, nodeId, StringComparison.OrdinalIgnoreCase)
                        || t.NodeId.StartsWith(nodeId + ".", StringComparison.OrdinalIgnoreCase))
            .ToList();

    /// <summary>The label to show for a tunable inside its node: the binding field, or "val" for a value node.</summary>
    private static string TunableKey(string ownerNodeId, LiveValue t) =>
        t.NodeId.Length > ownerNodeId.Length && t.NodeId.StartsWith(ownerNodeId + ".", StringComparison.OrdinalIgnoreCase)
            ? t.NodeId[(ownerNodeId.Length + 1)..]
            : "val";

    /// <summary>Compact display of a JSON value: bare text for strings, JSON for everything else.</summary>
    private static string ValueText(JsonNode? node) => node switch
    {
        null => "—",
        JsonValue v when v.TryGetValue(out string? s) => s,
        _ => node.ToJsonString()
    };

    /// <summary>
    /// One config line per node for drawing inside its box. Live tunables win — their current values are the
    /// interesting, changing part; a node with none falls back to a couple of its static scalar settings.
    /// </summary>
    private IReadOnlyDictionary<string, GraphRenderer.NodeConfigLine> BuildConfigLines()
    {
        var lines = new Dictionary<string, GraphRenderer.NodeConfigLine>(StringComparer.OrdinalIgnoreCase);

        foreach (var node in _definition.Nodes)
        {
            var tunables = NodeTunables(node.Id);
            if (tunables.Count > 0)
            {
                var text = string.Join(" ", tunables.Select(t => $"{TunableKey(node.Id, t)}={ValueText(t.Current)}"));
                lines[node.Id] = new GraphRenderer.NodeConfigLine(text, Tunable: true);
                continue;
            }

            // Static fallback: the first couple of scalar config entries, so even a plain node shows what it
            // was set up with. Objects/arrays are skipped — they never fit one line and read as noise.
            var scalars = node.Config
                .Where(p => p.Value is JsonValue)
                .Take(2)
                .Select(p => $"{p.Key}={ValueText(p.Value)}")
                .ToList();

            if (scalars.Count > 0)
                lines[node.Id] = new GraphRenderer.NodeConfigLine(string.Join(" ", scalars), Tunable: false);
        }

        return lines;
    }

    /// <summary>The bottom control bar: colour legend, the selected node, and the key hints.</summary>
    private IRenderable BuildControlsBar()
    {
        var selected = SelectedNodeId is { } id && _state.Nodes.TryGetValue(id, out var s)
            ? $"[grey]sel:[/][deepskyblue1]{Markup.Escape(s.DisplayName)}[/]"
            : "[grey]sel:—[/]";

        var legend =
            "[deepskyblue1]●[/][grey46]src[/] " +
            "[mediumpurple2]●[/][grey46]compute[/] " +
            "[hotpink]●[/][grey46]classify[/] " +
            "[gold1]●[/][grey46]flow[/] " +
            "[mediumspringgreen]●[/][grey46]sink[/] " +
            "[aquamarine1]●[/][grey46]value[/]";

        var pause = _hasLoop ? " [grey42]· space:pause[/]" : string.Empty;
        var controls = $"[grey42]←→ move · enter details[/]{pause}";

        var line = $"{selected}    {legend}    {controls}";

        return _editNotice is null
            ? new Markup(line)
            : new Rows(new Markup(line), new Markup($"  {_editNotice}"));
    }

    // ── Rendering ───────────────────────────────────────────────────────────

    private void RenderFrame()
    {
        // Keep the cursor riding the executing node until the operator takes over — the viewport follows it.
        SyncCursorToActive();
        PaintInPlace(BuildLayout());
    }

    /// <summary>
    /// Draws a renderable at the top-left without a full clear (which flashes), then blanks the rest of the
    /// window so the previous, possibly taller, frame does not bleed through. Shared by the dashboard and the
    /// node detail view.
    /// </summary>
    private static void PaintInPlace(IRenderable renderable)
    {
        try { Console.SetCursorPosition(0, 0); } catch { }

        AnsiConsole.Write(renderable);

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

    /// <summary>The layer the viewport centres on — the selected node's, which auto-follows execution.</summary>
    private int AnchorLayer() => SelectedNodeId is { } id ? _layout.LayerOf(id) : 0;

    private IRenderable BuildLayout()
    {
        var width    = Console.WindowWidth > 0 ? Console.WindowWidth : 120;
        var snapshot = _host.GetSnapshot();

        var rows = new List<IRenderable>
        {
            BuildHeader(snapshot),
            new Rule { Style = Style.Parse("grey23") },
            GraphRenderer.Render(
                _layout,
                _state.Nodes,
                BuildConfigLines(),
                SelectedNodeId,
                _state.LastActiveNodeId,
                width,
                AnchorLayer())
        };

        rows.Add(new Rule { Style = Style.Parse("grey23") });
        rows.Add(BuildControlsBar());

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

        // Per-cycle timing: total wall clock alone hides whether the graph is keeping up. avg is elapsed
        // over completed cycles; last is the most recent cycle's wall clock. Both blank until a cycle lands.
        var avgCycle  = _state.AverageCycleDuration;
        var lastCycle = _state.LastCycleDuration;
        var cycleCell = snapshot.TotalCycles > 0
            ? $"  [grey]cyc̄:[/][grey58]{avgCycle.TotalMilliseconds:F0}ms[/]" +
              (lastCycle > TimeSpan.Zero ? $" [grey42](last {lastCycle.TotalMilliseconds:F0}ms)[/]" : string.Empty)
            : string.Empty;

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
            cycleCell +
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

    // ── Node detail view ───────────────────────────────────────────────────────

    /// <summary>
    /// Takes over the screen with a single node's page — its config, live stats and recent log — and lets
    /// the operator edit its tunable fields in place. The run keeps going the whole time (the executor is a
    /// separate task); the page repaints every refresh so the stats and log stay live while it is open.
    /// </summary>
    private void OpenNodeDetail(string nodeId)
    {
        if (!_state.Nodes.ContainsKey(nodeId)) return;

        var fieldIndex = 0;
        try { Console.CursorVisible = false; } catch { }
        Console.Clear();

        while (true)
        {
            var tunables = NodeTunables(nodeId);
            fieldIndex = tunables.Count == 0 ? 0 : Math.Clamp(fieldIndex, 0, tunables.Count - 1);

            PaintInPlace(BuildNodeDetail(nodeId, tunables, fieldIndex));

            // No key within the window → fall through and repaint, so live stats/logs keep updating.
            if (!WaitForKey(RefreshMs, out var key)) continue;

            switch (key.Key)
            {
                case ConsoleKey.Escape:
                case ConsoleKey.Q:
                case ConsoleKey.LeftArrow:
                case ConsoleKey.Backspace:
                    Console.Clear();
                    return;

                case ConsoleKey.Spacebar when _hasLoop && _liveValues is not null:
                    _liveValues.RunControl.Toggle();
                    break;

                case ConsoleKey.UpArrow:
                    if (tunables.Count > 0) fieldIndex = (fieldIndex - 1 + tunables.Count) % tunables.Count;
                    break;

                case ConsoleKey.DownArrow:
                case ConsoleKey.Tab:
                    if (tunables.Count > 0) fieldIndex = (fieldIndex + 1) % tunables.Count;
                    break;

                case ConsoleKey.Enter:
                    if (tunables.Count > 0) EditSelected(tunables[fieldIndex]);
                    break;
            }
        }
    }

    /// <summary>Blocks up to <paramref name="withinMs"/> for a keypress. False means the window elapsed.</summary>
    private static bool WaitForKey(int withinMs, out ConsoleKeyInfo key)
    {
        key = default;
        var deadline = Environment.TickCount64 + withinMs;
        while (Environment.TickCount64 < deadline)
        {
            try
            {
                if (Console.KeyAvailable)
                {
                    key = Console.ReadKey(intercept: true);
                    return true;
                }
            }
            catch { return false; }   // redirected input — no keyboard
            Thread.Sleep(15);
        }
        return false;
    }

    private IRenderable BuildNodeDetail(string nodeId, IReadOnlyList<LiveValue> tunables, int fieldIndex)
    {
        var def = _definition.Nodes.FirstOrDefault(n => string.Equals(n.Id, nodeId, StringComparison.OrdinalIgnoreCase));
        _state.Nodes.TryGetValue(nodeId, out var st);

        var title = new Markup(
            $"[bold deepskyblue1]◈ NODE[/]  [bold]{Markup.Escape(st?.DisplayName ?? nodeId)}[/]   " +
            $"[grey]id:[/][grey58]{Markup.Escape(nodeId)}[/]   " +
            $"[grey]kind:[/][grey58]{Markup.Escape(st?.Kind ?? "-")}[/]   " +
            $"[grey]type:[/][grey58]{Markup.Escape(st?.TypeLabel ?? "-")}[/]   " +
            $"[grey]cat:[/][grey58]{Markup.Escape(def?.Category ?? st?.Category ?? "-")}[/]");

        var top = new Grid();
        top.AddColumn(new GridColumn());
        top.AddColumn(new GridColumn());
        top.AddRow(BuildEditablePanel(nodeId, tunables, fieldIndex), BuildStatsPanel(st));

        var controls = tunables.Count > 0
            ? "[grey42]↑↓ field · enter edit · esc/← back[/]"
            : "[grey42]esc/← back[/]";
        if (_editNotice is not null) controls += $"    {_editNotice}";

        return new Rows(
            title,
            new Rule { Style = Style.Parse("grey23") },
            top,
            BuildConfigPanel(def),
            BuildNodeLogPanel(nodeId),
            new Markup(controls));
    }

    private IRenderable BuildEditablePanel(string nodeId, IReadOnlyList<LiveValue> tunables, int fieldIndex)
    {
        IRenderable body;
        if (tunables.Count == 0)
        {
            body = new Markup("[grey42]no editable fields[/]");
        }
        else
        {
            var lines = tunables.Select((t, i) =>
            {
                var key    = TunableKey(nodeId, t);
                var val    = ValueText(t.Current);
                var type   = ControlValueTypes.ToToken(t.Type);
                var origin = t.Binding is { Length: > 0 } b
                    ? $"[grey42]→ {Markup.Escape(b)}[/]"
                    : "[grey42](run-only)[/]";
                var cursor = i == fieldIndex ? "[deepskyblue1]▶[/] " : "  ";
                var cell   = i == fieldIndex
                    ? $"[black on deepskyblue1] {Markup.Escape(key)}={Markup.Escape(val)} [/]"
                    : $"[grey]{Markup.Escape(key)}=[/][white]{Markup.Escape(val)}[/]";
                return $"{cursor}{cell} [grey42]{Markup.Escape(type)}[/]  {origin}";
            });
            body = new Markup(string.Join("\n", lines));
        }

        return new Panel(body)
        {
            Header = new PanelHeader("[gold1] editable [/]"),
            Border = BoxBorder.Rounded,
            BorderStyle = Style.Parse("gold1"),
            Expand = true
        };
    }

    private static IRenderable BuildStatsPanel(PipelineNodeState? st)
    {
        IRenderable body;
        if (st is null)
        {
            body = new Markup("[grey42]no stats yet[/]");
        }
        else
        {
            var statusText = st.Status switch
            {
                NodeLifecycleStatus.Faulted => "[red]faulted[/]",
                NodeLifecycleStatus.Done    => "[green]done[/]",
                NodeLifecycleStatus.Active  => "[yellow]active[/]",
                _                           => "[grey]idle[/]"
            };
            var lastMs = st.LastDurationMicros / 1000.0;
            var avgMs  = st.AverageDurationMicros / 1000.0;
            var minMs  = st.MinDurationMicros == long.MaxValue ? 0 : st.MinDurationMicros / 1000.0;
            var maxMs  = st.MaxDurationMicros / 1000.0;
            var inP    = st.LastInputPorts.Count  > 0 ? string.Join(",", st.LastInputPorts)  : "—";
            var outP   = st.LastOutputPorts.Count > 0 ? string.Join(",", st.LastOutputPorts) : "—";

            body = new Markup(string.Join("\n",
            [
                $"[grey]status  [/] {statusText}",
                $"[grey]cycles  [/] [white]{st.TotalCycles}[/]  [grey]ok[/] [green]{st.AcceptedCycles}[/]  [grey]fault[/] [red]{st.FaultedCycles}[/]",
                $"[grey]last    [/] {lastMs:F2}ms",
                $"[grey]avg/min/max[/] {avgMs:F2} / {minMs:F2} / {maxMs:F2} ms",
                $"[grey]restarts[/] [red]{st.WorkerRestarts}[/]",
                $"[grey]in  →   [/] [steelblue1]{Markup.Escape(inP)}[/]",
                $"[grey]out →   [/] [mediumspringgreen]{Markup.Escape(outP)}[/]"
            ]));
        }

        return new Panel(body)
        {
            Header = new PanelHeader("[steelblue1] stats [/]"),
            Border = BoxBorder.Rounded,
            BorderStyle = Style.Parse("steelblue1"),
            Expand = true
        };
    }

    private static IRenderable BuildConfigPanel(PipelineNodeDefinition? def)
    {
        IRenderable body;
        if (def is null || def.Config.Count == 0)
        {
            body = new Markup("[grey42](no config)[/]");
        }
        else
        {
            var lines = def.Config.Select(p =>
                $"[grey66]{Markup.Escape(p.Key)}[/][grey42] = [/][white]{Markup.Escape(p.Value?.ToJsonString() ?? "null")}[/]");
            body = new Markup(string.Join("\n", lines));
        }

        return new Panel(body)
        {
            Header = new PanelHeader("[grey70] config [/]"),
            Border = BoxBorder.Rounded,
            BorderStyle = Style.Parse("grey35"),
            Expand = true
        };
    }

    private IRenderable BuildNodeLogPanel(string nodeId)
    {
        var logs = _state.GetNodeLogs(nodeId, 8);
        IRenderable body = logs.Count == 0
            ? new Markup("[grey42](no events yet…)[/]")
            : new Markup(string.Join("\n", logs.Select(l =>
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
            })));

        return new Panel(body)
        {
            Header = new PanelHeader("[grey70] logs [/]"),
            Border = BoxBorder.Rounded,
            BorderStyle = Style.Parse("grey35"),
            Expand = true
        };
    }
}


