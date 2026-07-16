using MachineVisionFabric.Core.Abstractions;
using MachineVisionFabric.Contracts.Packages;
using Microsoft.Extensions.Logging;
using Microsoft.Extensions.Options;

namespace MachineVisionFabric.Runtime;

public sealed class ProfileProductPresenceGateResolver(
    IOptions<MachineVisionFabricRuntimeOptions> options,
    IIntegrationModuleLoader integrationModuleLoader,
    ILogger<ProfileProductPresenceGateResolver> logger) : IProductPresenceGateResolver
{
    public ProductPresenceGateResolution Resolve(FabricProfileManifest manifest, string integrationsRoot)
    {
        var binding = manifest.ProductPresenceGate;
        if (string.Equals(binding.Mode, "module", StringComparison.OrdinalIgnoreCase))
        {
            var modules = integrationModuleLoader.LoadModules(integrationsRoot)
                .OfType<IProductPresenceGateModule>()
                .ToArray();

            var selectedModule = modules.FirstOrDefault(module =>
                string.Equals(module.Describe().ModuleId, binding.ModuleId, StringComparison.OrdinalIgnoreCase));

            if (selectedModule is null)
            {
                throw new InvalidOperationException(
                    $"Product presence gate module '{binding.ModuleId}' could not be found under '{integrationsRoot}'.");
            }

            logger.LogInformation(
                "Resolved product presence gate through external module {ModuleId}.",
                selectedModule.Describe().ModuleId);

            return new ProductPresenceGateResolution(
                selectedModule.CreateGate(binding.Config),
                "module",
                selectedModule.Describe().ModuleId);
        }

        logger.LogInformation("Resolved product presence gate through built-in simulated gate.");

        return new ProductPresenceGateResolution(
            new SimulatedProductPresenceGate(options.Value.PlcGate),
            "builtin",
            options.Value.PlcGate.SourceName);
    }
}
