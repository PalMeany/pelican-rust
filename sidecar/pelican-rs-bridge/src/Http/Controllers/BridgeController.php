<?php

namespace PelicanRs\Bridge\Http\Controllers;

use App\Extensions\Avatar\AvatarService;
use App\Extensions\Features\FeatureService;
use App\Extensions\Tasks\TaskService;
use App\Models\ApiKey;
use App\Models\Plugin;
use App\Models\Role;
use App\Models\Subuser;
use App\Models\Task;
use App\Services\Helpers\PluginService;
use BackedEnum;
use Filament\Facades\Filament;
use Illuminate\Database\Eloquent\Model;
use Illuminate\Http\JsonResponse;
use Illuminate\Http\Request;
use Illuminate\Routing\Controller;
use Illuminate\Support\Facades\Route;
use PelicanRs\Bridge\Origin;
use ReflectionProperty;
use Throwable;

class BridgeController extends Controller
{
    /** What PHP plugins registered: routes, panel navigation, listened events, registries. */
    public function manifest(): JsonResponse
    {
        return new JsonResponse([
            'version' => config('app.version'),
            'plugins' => array_values(Origin::namespaces()),
            'routes' => $this->routes(),
            'framework_routes' => $this->frameworkRoutes(),
            'navigation' => $this->navigation(),
            'listeners' => $this->listenedEvents(),
            'tasks' => $this->tasks(),
            'features' => $this->features(),
            'avatars' => $this->avatars(),
            'admin_permissions' => $this->staticProperty(Role::class, 'customPermissions'),
            'subuser_permissions' => array_values(array_map(fn ($group) => [
                'name' => $group['name'],
                'permissions' => $group['permissions'],
                'icon' => $this->iconName($group['icon'] ?? null),
                'hidden' => $group['hidden'] ?? false,
                'translation_prefix' => $group['translation_prefix'] ?? null,
            ], $this->staticProperty(Subuser::class, 'customPermissions'))),
            'api_resources' => array_values($this->staticProperty(ApiKey::class, 'customResourceNames')),
        ]);
    }

    /** An event from pelican-rs, delivered to the plugins' listeners only. */
    public function event(Request $request): JsonResponse
    {
        $name = (string) $request->input('event');
        $payload = match (true) {
            str_starts_with($name, 'eloquent.') => [$this->eloquentModel($name, (array) $request->input('model'))],
            default => [$this->eventObject($name, (array) $request->input('args', []))],
        };
        $eventName = str_starts_with($name, 'eloquent.') ? $name : get_class($payload[0]);

        $dispatcher = app('events');
        $called = 0;
        foreach ($this->rawListeners($eventName) as [$listener, $wildcard]) {
            if (is_null(Origin::pluginOfListener($listener))) {
                continue;
            }
            // makeListener() wrappers take ($event, $payload) and spread it for non-wildcards.
            $dispatcher->makeListener($listener, $wildcard)($eventName, $payload);
            $called++;
        }

        return new JsonResponse(['listeners' => $called]);
    }

    /** Run a schedule task whose type a PHP plugin registered. */
    public function task(Request $request, TaskService $tasks): JsonResponse
    {
        $task = Task::query()->findOrFail((int) $request->input('task'));
        $schema = $tasks->get($task->action);
        abort_if(is_null($schema) || is_null(Origin::pluginOfClass($schema::class)), 404);

        try {
            $schema->runTask($task);
        } catch (Throwable $exception) {
            return new JsonResponse(['error' => $exception->getMessage()], 500);
        }

        return new JsonResponse(['ok' => true]);
    }

    /** Plugin lifecycle steps that need PHP (composer packages, migrations, seeders). */
    public function artisan(Request $request, PluginService $service): JsonResponse
    {
        $plugin = Plugin::query()->findOrFail((string) $request->input('plugin'));

        try {
            match ((string) $request->input('action')) {
                'install' => $service->installPlugin($plugin, (bool) $request->input('enable', true)),
                'update' => $service->updatePlugin($plugin),
                'uninstall' => $service->uninstallPlugin($plugin, (bool) $request->input('delete', false)),
                'enable' => $service->enablePlugin($plugin),
                'disable' => $service->disablePlugin($plugin),
                default => abort(422),
            };
        } catch (Throwable $exception) {
            return new JsonResponse(['error' => $exception->getMessage()], 500);
        }

        return new JsonResponse(['ok' => true]);
    }

    /** @return array<int, array<string, mixed>> */
    private function routes(): array
    {
        $out = [];
        foreach (Route::getRoutes()->getRoutes() as $route) {
            $uses = $route->getAction('uses');
            $plugin = $uses instanceof \Closure
                ? Origin::pluginOfListener($uses)
                : Origin::pluginOfClass(explode('@', (string) $route->getActionName())[0]);
            if (is_null($plugin)) {
                continue;
            }
            $out[] = [
                'plugin' => $plugin,
                'methods' => array_values(array_diff($route->methods(), ['HEAD'])),
                'uri' => '/' . ltrim($route->uri(), '/'),
                'name' => $route->getName(),
            ];
        }

        return $out;
    }

    /**
     * Livewire's endpoints (hashed prefix in Livewire 4) and Filament's export/import
     * downloads: needed by the plugins' pages, owned by neither panel's core.
     *
     * @return string[]
     */
    private function frameworkRoutes(): array
    {
        $out = [];
        foreach (Route::getRoutes()->getRoutes() as $route) {
            $action = ltrim((string) $route->getActionName(), '\\');
            if (str_starts_with($action, 'Livewire\\') || str_starts_with($action, 'Filament\\Actions\\')) {
                $out[] = '/' . ltrim($route->uri(), '/');
            }
        }

        return array_values(array_unique($out));
    }

    /** @return array<int, array<string, mixed>> */
    private function navigation(): array
    {
        $out = [];
        foreach (Filament::getPanels() as $panel) {
            foreach ([...$panel->getPages(), ...$panel->getResources()] as $class) {
                $plugin = Origin::pluginOfClass($class);
                if (is_null($plugin) || !$class::shouldRegisterNavigation()) {
                    continue;
                }
                try {
                    $out[] = [
                        'plugin' => $plugin,
                        'panel' => $panel->getId(),
                        'path' => '/' . trim($panel->getPath() . '/' . $class::getSlug($panel), '/'),
                        'tenant' => $panel->hasTenancy(),
                        'label' => $class::getNavigationLabel(),
                        'group' => $this->label($class::getNavigationGroup()),
                        'icon' => $this->iconName($class::getNavigationIcon()),
                        'sort' => $class::getNavigationSort(),
                    ];
                } catch (Throwable) {
                    // A page needing a request context for its navigation: skipped.
                }
            }
        }

        return $out;
    }

    /** Event names with at least one plugin listener (wildcards as registered). */
    private function listenedEvents(): array
    {
        $dispatcher = app('events');
        $names = [];
        foreach ($dispatcher->getRawListeners() as $name => $listeners) {
            foreach ($listeners as $listener) {
                if (!is_null(Origin::pluginOfListener($listener))) {
                    $names[] = $name;
                    break;
                }
            }
        }
        foreach ((new ReflectionProperty($dispatcher, 'wildcards'))->getValue($dispatcher) as $name => $listeners) {
            foreach ($listeners as $listener) {
                if (!is_null(Origin::pluginOfListener($listener))) {
                    $names[] = $name;
                    break;
                }
            }
        }

        return array_values(array_unique($names));
    }

    /** @return array<int, array{0: mixed, 1: bool}> */
    private function rawListeners(string $eventName): array
    {
        $dispatcher = app('events');
        $out = [];
        foreach ($dispatcher->getRawListeners()[$eventName] ?? [] as $listener) {
            $out[] = [$listener, false];
        }
        foreach ((new ReflectionProperty($dispatcher, 'wildcards'))->getValue($dispatcher) as $pattern => $listeners) {
            if (str($eventName)->is($pattern)) {
                foreach ($listeners as $listener) {
                    $out[] = [$listener, true];
                }
            }
        }

        return $out;
    }

    /** `eloquent.<action>: <class>` model: re-read by key, or rebuilt from attributes (deleted). */
    private function eloquentModel(string $name, array $ref): Model
    {
        $class = trim(str($name)->after(': ')->toString());
        abort_unless(is_subclass_of($class, Model::class), 422);
        if (!str_starts_with($name, 'eloquent.deleted') && isset($ref['id'])) {
            $model = $class::query()->find($ref['id']);
            if (!is_null($model)) {
                return $model;
            }
        }
        $model = (new $class())->newFromBuilder($ref['attributes'] ?? []);
        $model->exists = !str_starts_with($name, 'eloquent.deleted');

        return $model;
    }

    /** An `App\Events\*` object from constructor arguments (`{model, id}` references or scalars). */
    private function eventObject(string $class, array $args): object
    {
        abort_unless(str_starts_with($class, 'App\\Events\\') && class_exists($class), 422);
        $values = array_map(function ($arg) {
            if (is_array($arg) && isset($arg['model'], $arg['id']) && is_subclass_of($arg['model'], Model::class)) {
                return $arg['model']::query()->findOrFail($arg['id']);
            }

            return $arg;
        }, $args);

        return new $class(...$values);
    }

    /** @return array<int, array<string, mixed>> */
    private function tasks(): array
    {
        $out = [];
        foreach (app(TaskService::class)->getAll() as $schema) {
            if ($plugin = Origin::pluginOfClass($schema::class)) {
                $out[] = [
                    'plugin' => $plugin,
                    'id' => $schema->getId(),
                    'name' => $schema->getName(),
                    'payload_label' => $schema->getPayloadLabel(),
                    'default_payload' => $schema->getDefaultPayload(),
                ];
            }
        }

        return $out;
    }

    /** @return array<int, array<string, mixed>> */
    private function features(): array
    {
        $out = [];
        foreach (app(FeatureService::class)->getAll() as $schema) {
            if ($plugin = Origin::pluginOfClass($schema::class)) {
                $out[] = ['plugin' => $plugin, 'id' => $schema->getId(), 'listeners' => $schema->getListeners()];
            }
        }

        return $out;
    }

    /** @return array<int, array<string, mixed>> */
    private function avatars(): array
    {
        $out = [];
        $schemas = (new ReflectionProperty(AvatarService::class, 'schemas'))->getValue(app(AvatarService::class));
        foreach ($schemas as $schema) {
            if ($plugin = Origin::pluginOfClass($schema::class)) {
                $out[] = ['plugin' => $plugin, 'id' => $schema->getId(), 'name' => $schema->getName()];
            }
        }

        return $out;
    }

    private function staticProperty(string $class, string $property): array
    {
        return (array) (new ReflectionProperty($class, $property))->getValue();
    }

    private function iconName(mixed $icon): ?string
    {
        return match (true) {
            $icon instanceof BackedEnum => (string) $icon->value,
            is_string($icon) => $icon,
            default => null,
        };
    }

    private function label(mixed $value): ?string
    {
        return match (true) {
            $value instanceof BackedEnum => (string) $value->value,
            $value instanceof \UnitEnum => $value->name,
            is_null($value) => null,
            default => (string) $value,
        };
    }
}
