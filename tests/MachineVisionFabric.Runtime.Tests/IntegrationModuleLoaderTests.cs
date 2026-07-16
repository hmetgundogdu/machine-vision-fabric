using MachineVisionFabric.Runtime.Plugins;

namespace MachineVisionFabric.Runtime.Tests;

public sealed class IntegrationModuleLoaderTests
{
    [Fact]
    public void LoadModules_UsesIntegrationManifestDiscovery()
    {
        var repositoryRoot = ResolveRepositoryRoot();
        var integrationsRoot = Path.Combine(repositoryRoot, "examples", "integrations");
        var loader = new IntegrationModuleLoader();

        var modules = loader.LoadModules(integrationsRoot);
        var moduleIds = modules
            .Select(module => module.Describe().ModuleId)
            .OrderBy(id => id, StringComparer.OrdinalIgnoreCase)
            .ToArray();

        Assert.Contains("mvf.folder-source", moduleIds);
        Assert.Contains("mvf.resident-camera-stub", moduleIds);
        Assert.Contains("mvf.s7-gateway-gate", moduleIds);
        Assert.Contains("mvf.simulated-gate", moduleIds);
        Assert.Contains("mvf.tcp-plc-gate", moduleIds);
    }

    private static string ResolveRepositoryRoot()
    {
        var currentDirectory = new DirectoryInfo(AppContext.BaseDirectory);
        while (currentDirectory is not null)
        {
            var hasSrc = Directory.Exists(Path.Combine(currentDirectory.FullName, "src"));
            var hasExampleIntegrations = Directory.Exists(Path.Combine(currentDirectory.FullName, "examples", "integrations"));
            if (hasSrc && hasExampleIntegrations)
            {
                return currentDirectory.FullName;
            }

            currentDirectory = currentDirectory.Parent;
        }

        throw new DirectoryNotFoundException("Repository root could not be resolved for tests.");
    }
}
