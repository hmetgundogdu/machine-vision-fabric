using System.Text.Json.Nodes;
using Mvf.Abstractions;
using Mvf.Graph.Values;
using Spectre.Console;

namespace Mvf.Cli.Values;

/// <summary>
/// Asks the operator, on the terminal, before the run starts.
///
/// <para>It renders <b>a type</b>, never a layout: a string gets a text entry, a list of choices gets a
/// picker. The graph describes no widgets — the moment a node describes widgets it stops being a
/// dataflow description.</para>
///
/// <para><see cref="CanResolve"/> is false whenever there is no one to ask (stdin redirected, no ANSI
/// terminal, or <c>--no-prompt</c>), so an unattended run fails with a binding name to set rather than
/// blocking forever on a prompt nobody will see.</para>
/// </summary>
public sealed class TerminalValueResolver(bool enabled = true) : IValueResolver
{
    public bool CanResolve => enabled && !Console.IsInputRedirected && AnsiConsole.Profile.Capabilities.Interactive;

    public Task<ValueResolution> ResolveAsync(ValueRequest request, CancellationToken cancellationToken)
    {
        var label = request.Prompt
            ?? (request.Binding is { Length: > 0 } binding
                ? $"Value for '{binding}'"
                : $"Value for node '{request.NodeId}'");

        if (request.Choices is { Count: > 0 })
        {
            return Task.FromResult(PickFromChoices(label, request));
        }

        var typeHint = request.Shape == ControlValueShape.List
            ? $"JSON list of {ControlValueTypes.ToToken(request.Type)}"
            : ControlValueTypes.ToToken(request.Type);

        // The default is printed on its own line rather than handed to DefaultValue, and never spliced into
        // the prompt string: Spectre renders both as markup, and a JSON value's '[' would be read as a
        // style tag and throw. Enter-means-default is implemented below instead.
        var fallback = request.Default is { } declared ? AsPromptText(declared, request) : null;
        if (fallback is not null)
        {
            AnsiConsole.MarkupLine($"[grey]default:[/] [grey58]{Markup.Escape(fallback)}[/]");
        }

        var suffix = fallback is null ? string.Empty : " [grey58](enter accepts it)[/]";

        // Both checks live in the prompt's own validation so a mistyped answer is asked again, instead of
        // being accepted here and failing the whole run in the pre-pass a moment later.
        var prompt = new TextPrompt<string>(
                $"[green]?[/] {Markup.Escape(label)} [grey]({Markup.Escape(typeHint)})[/]{suffix}:")
            .AllowEmpty()
            .Validate(text => Check(text, request, typeHint));

        var answer = AnsiConsole.Prompt(prompt);
        if (answer.Length == 0 && fallback is not null)
        {
            answer = fallback;
        }

        if (!ControlValueTypes.TryParseShaped(request.Shape, request.Type, answer, out var value))
        {
            return Task.FromResult(ValueResolution.Failed($"'{answer}' is not a valid {typeHint}."));
        }

        return Task.FromResult(ValueResolution.Ok(value));
    }

    /// <summary>
    /// Type, shape and schema, in the order that gives the most useful message. An empty answer passes:
    /// it means "take the default", which the pre-pass then checks like any other value.
    /// </summary>
    private static ValidationResult Check(string text, ValueRequest request, string typeHint)
    {
        if (text.Length == 0)
        {
            return ValidationResult.Success();
        }

        if (!ControlValueTypes.TryParseShaped(request.Shape, request.Type, text, out var candidate))
        {
            return ValidationResult.Error($"[red]not a valid {Markup.Escape(typeHint)}[/]");
        }

        return JsonSchemaCheck.TryValidateShaped(request.Shape, request.Schema, candidate, out var schemaError)
            ? ValidationResult.Success()
            : ValidationResult.Error($"[red]{Markup.Escape(schemaError ?? "does not match the declared schema")}[/]");
    }

    /// <summary>
    /// How a default is spelled back to the operator, in the same syntax the answer is read in: a plain
    /// <c>string</c> value is bare text, everything else is JSON. Getting this wrong would pre-fill a
    /// string prompt with a quoted string and quietly store the quotes.
    /// </summary>
    private static string AsPromptText(JsonNode value, ValueRequest request) =>
        request is { Shape: ControlValueShape.Single, Type: ControlValueType.String }
            ? value.GetValue<string>()
            : value.ToJsonString();


    /// <summary>
    /// The picker: a discovered collection rendered as a list, with the chosen element (or the property
    /// named by <c>by</c>) stored under the binding. Every later run reads the binding and never asks.
    /// </summary>
    private static ValueResolution PickFromChoices(string label, ValueRequest request)
    {
        var choices = request.Choices!;
        var labels = choices.Select((choice, index) => DescribeChoice(choice, index)).ToList();

        // UseConverter rather than escaping the labels themselves: Spectre renders each choice as markup,
        // and a record with a '[' in it would be read as a style tag — but the picked string has to come
        // back unescaped so it can be matched against the list.
        var picked = AnsiConsole.Prompt(
            new SelectionPrompt<string>()
                .Title($"[green]?[/] {Markup.Escape(label)}")
                .UseConverter(Markup.Escape)
                .AddChoices(labels));

        var chosen = choices[labels.IndexOf(picked)];

        // With a `by`, what is stored is the identifying property, not the whole record — that is what
        // makes the binding survive a discovery run that returns the same camera with different
        // incidental fields.
        if (request.ChoiceLabelProperty is { Length: > 0 } property
            && chosen is JsonObject obj
            && obj.TryGetPropertyValue(property, out var identifier))
        {
            return ValueResolution.Ok(identifier?.DeepClone());
        }

        return ValueResolution.Ok(chosen?.DeepClone());
    }

    private static string DescribeChoice(JsonNode? choice, int index) => choice switch
    {
        null => $"#{index + 1} (null)",
        JsonObject obj => string.Join("  ", obj.Take(3).Select(p => $"{p.Key}={p.Value?.ToJsonString() ?? "null"}")),
        _ => choice.ToJsonString()
    };
}
