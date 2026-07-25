using Mvf.Abstractions;
using Mvf.Graph.Values;

namespace Mvf.Engine.Values;

/// <summary>
/// Reads a binding from an environment variable — <c>MVF_BINDING_&lt;NAME&gt;</c>, with dots and dashes
/// folded to underscores and the whole thing upper-cased, so binding <c>camera.address</c> is
/// <c>MVF_BINDING_CAMERA_ADDRESS</c>.
///
/// <para>This is how a value is supplied on a machine with no operator: a service definition, a
/// container env file, or a deployment script sets it, and the run never asks anyone anything.</para>
/// </summary>
public sealed class EnvironmentValueResolver : IValueResolver
{
    public const string Prefix = "MVF_BINDING_";

    public bool CanResolve => true;

    public Task<ValueResolution> ResolveAsync(ValueRequest request, CancellationToken cancellationToken)
    {
        if (request.Binding is not { Length: > 0 } binding)
        {
            return Task.FromResult(ValueResolution.Unresolved);
        }

        var variable = VariableNameFor(binding);
        var text = Environment.GetEnvironmentVariable(variable);
        if (text is null)
        {
            return Task.FromResult(ValueResolution.Unresolved);
        }

        if (!ControlValueTypes.TryParseValue(request.Type, text, out var value))
        {
            return Task.FromResult(ValueResolution.Failed(
                $"{variable} is set to '{text}', which is not a valid {ControlValueTypes.ToToken(request.Type)}."));
        }

        // Transient on purpose: the variable is already the durable source, and the binding store is read
        // before any resolver — caching this would make the first run's value outrank every later change.
        return Task.FromResult(ValueResolution.Transient(value));
    }

    public static string VariableNameFor(string binding) =>
        Prefix + binding.Replace('.', '_').Replace('-', '_').ToUpperInvariant();
}
