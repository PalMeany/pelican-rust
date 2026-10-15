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
            'render_hooks' => array_keys($this->pluginRenderHooks()),
            'boots' => $this->pluginsBootPanels(),
            'admin_widgets' => count(\PelicanRs\Bridge\Filament\WidgetsHost::pluginWidgets(Filament::getPanel('admin'))),
            'admin_relations' => (object) array_map('count', \PelicanRs\Bridge\Filament\RelationsHost::pluginRelations()),
            'console_actions' => array_map(fn ($a) => $a->getName(), \PelicanRs\Bridge\Filament\ConsoleActionHost::pluginHeaderActions()),
            'tasks' => $this->tasks(),
            'features' => $this->features(),
            'avatars' => $this->avatars(),
            'oauth_providers' => $this->oauthProviders(),
            'plugin_settings' => array_values(Plugin::all()->filter(fn (Plugin $p) => \PelicanRs\Bridge\Filament\PluginSettingsHost::configurable($p->id) !== null)->pluck('id')->all()),
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

    /**
     * Translations of enabled language plugins (`$translator->addPath()`): locale => group
     * (`server/feature`, `*` for JSON files) => lines.
     */
    public function lang(): JsonResponse
    {
        $out = [];
        foreach (Plugin::query()->orderBy('load_order')->get() as $plugin) {
            if ($plugin->status !== \App\Enums\PluginStatus::Enabled || !$plugin->isLanguage()) {
                continue;
            }
            $root = plugin_path($plugin->id, 'lang');
            foreach (\Illuminate\Support\Facades\File::directories($root) as $dir) {
                $locale = basename($dir);
                foreach (\Illuminate\Support\Facades\File::allFiles($dir) as $file) {
                    if ($file->getExtension() !== 'php') {
                        continue;
                    }
                    $group = str_replace('\\', '/', substr($file->getRelativePathname(), 0, -4));
                    $lines = require $file->getPathname();
                    if (is_array($lines)) {
                        $out[$locale][$group] = array_replace_recursive($out[$locale][$group] ?? [], $lines);
                    }
                }
                $json = "$root/$locale.json";
                if (is_file($json)) {
                    $out[$locale]['*'] = array_replace($out[$locale]['*'] ?? [], (array) json_decode((string) file_get_contents($json), true));
                }
            }
        }

        return new JsonResponse((object) $out);
    }

    /** A PHP plugin's avatar URL for a user. */
    public function avatar(Request $request, AvatarService $avatars): JsonResponse
    {
        $schema = $avatars->get((string) $request->input('provider'));
        abort_if(is_null($schema) || is_null(Origin::pluginOfClass($schema::class)), 404);
        $user = \App\Models\User::query()->findOrFail((int) $request->input('user'));

        return new JsonResponse(['url' => $schema->get($user)]);
    }

    /** Plugin lifecycle steps that need PHP (composer packages, migrations, seeders). */
    public function artisan(Request $request, PluginService $service): JsonResponse
    {
        // p:plugin:composer: every installed plugin's packages, no plugin of its own.
        if ($request->input('action') === 'composer') {
            try {
                $service->manageComposerPackages();
            } catch (Throwable $exception) {
                report($exception);

                return new JsonResponse(['error' => $exception->getMessage()], 500);
            }

            return new JsonResponse(['ok' => true]);
        }

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
            if (str_starts_with($action, 'Livewire\\') || str_starts_with($action, 'Filament\\Actions\\')
                || str_starts_with($action, 'PelicanRs\\Bridge\\Filament\\')) {
                $out[] = '/' . ltrim($route->uri(), '/');
            }
        }

        return array_values(array_unique($out));
    }

    /**
     * @param  \Filament\Panel[]|null  $panels
     * @return array<int, array<string, mixed>>
     */
    private function navigation(?array $panels = null, bool $checkAccess = false): array
    {
        $out = [];
        foreach ($panels ?? Filament::getPanels() as $panel) {
            foreach ([...$panel->getPages(), ...$panel->getResources()] as $class) {
                $plugin = Origin::pluginOfClass($class);
                if (is_null($plugin) || !$class::shouldRegisterNavigation()) {
                    continue;
                }
                try {
                    if ($checkAccess && method_exists($class, 'canAccess') && !$class::canAccess()) {
                        continue;
                    }
                } catch (Throwable) {
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
            if (is_array($arg) && isset($arg['model']) && is_subclass_of($arg['model'], Model::class)) {
                if (isset($arg['attributes'])) {
                    // Rows that may not be committed yet arrive as raw attributes.
                    $model = (new $arg['model']())->newFromBuilder($arg['attributes']);
                    $model->exists = true;

                    return $model;
                }
                if (isset($arg['id'])) {
                    return $arg['model']::query()->findOrFail($arg['id']);
                }
            }

            return $arg;
        }, $args);

        return new $class(...$values);
    }

    /**
     * The plugins' Filament render hooks (`FilamentView::registerRenderHook()`): name => scope => closures.
     *
     * @return array<string, array<string, \Closure[]>>
     */
    private function pluginRenderHooks(): array
    {
        $manager = app(\Filament\Support\View\ViewManager::class);
        $all = (new ReflectionProperty($manager, 'renderHooks'))->getValue($manager);
        $out = [];
        foreach ($all as $name => $scopes) {
            foreach ($scopes as $scope => $hooks) {
                foreach ($hooks as $hook) {
                    if (!is_null(Origin::pluginOfListener($hook))) {
                        $out[$name][$scope][] = $hook;
                    }
                }
            }
        }

        return $out;
    }

    /**
     * Whether a plugin does something when a panel boots (`SetUpPanel` on every panel request):
     * a `boot()` with a body (alert banners, hooks) or render hooks given to the panel.
     */
    private function pluginsBootPanels(): bool
    {
        foreach (Filament::getPanels() as $panel) {
            foreach ($panel->getPlugins() as $plugin) {
                if (is_null(Origin::pluginOfClass($plugin::class))) {
                    continue;
                }
                $boot = new \ReflectionMethod($plugin, 'boot');
                $lines = array_slice((array) file((string) $boot->getFileName()), $boot->getStartLine() - 1, $boot->getEndLine() - $boot->getStartLine() + 1);
                $body = implode('', $lines);
                $body = substr($body, (int) strpos($body, '{') + 1, (int) strrpos($body, '}') - (int) strpos($body, '{') - 1);
                $body = preg_replace(['~/\*.*?\*/~s', '~//[^\n]*~', '~#[^\n]*~'], '', $body);
                if (trim((string) $body) !== '') {
                    return true;
                }
            }
            if ($this->pluginPanelRenderHooks($panel) !== []) {
                return true;
            }
        }

        return false;
    }

    /**
     * Render hooks plugins gave the panel (`$panel->renderHook()`), registered only when it boots.
     *
     * @return array<string, array<string, \Closure[]>>
     */
    private function pluginPanelRenderHooks(\Filament\Panel $panel): array
    {
        $out = [];
        foreach ((new ReflectionProperty($panel, 'renderHooks'))->getValue($panel) as $name => $scopes) {
            foreach ($scopes as $scope => $hooks) {
                foreach ($hooks as $hook) {
                    if (!is_null(Origin::pluginOfListener($hook))) {
                        $out[$name][$scope][] = $hook;
                    }
                }
            }
        }

        return $out;
    }

    /**
     * What `SetUpPanel` does for the plugins on a panel request: their `boot()`, collecting the
     * alert banners they send (`AlertBanner::send()`, shown at the top of the page).
     *
     * @return array<int, array<string, mixed>>
     */
    private function bootPlugins(\Filament\Panel $panel): array
    {
        session()->forget('alert-banners');
        foreach ($panel->getPlugins() as $plugin) {
            if (is_null(Origin::pluginOfClass($plugin::class))) {
                continue;
            }
            try {
                $plugin->boot($panel);
            } catch (Throwable $exception) {
                report($exception);
            }
        }
        $banners = [];
        foreach ((array) session()->pull('alert-banners', []) as $banner) {
            $banners[] = [
                'id' => (string) ($banner['id'] ?? ''),
                'title' => filled($banner['title'] ?? null) ? (string) str($banner['title'])->sanitizeHtml() : null,
                'body' => filled($banner['body'] ?? null) ? (string) str($banner['body'])->sanitizeHtml() : null,
                'status' => $banner['status'] ?? 'info',
                'icon' => $this->iconName($banner['icon'] ?? null),
                'closeable' => (bool) ($banner['closeable'] ?? false),
            ];
        }

        return $banners;
    }

    /**
     * What pelican-rs shows from the plugins on one page of `panel` (on server `tenant`), as the
     * signed user (or a guest) sees it, in one call: menu entries (Filament hides pages whose
     * `canAccess()` denies), the HTML of render hooks (global, scoped to the panel, given to the
     * panel), the alert banners the plugins' `boot()` sends, and on server pages the console
     * actions.
     */
    public function page(Request $request): JsonResponse
    {
        $user = \App\Models\User::query()->find((int) $request->header('X-Pelican-Rs-User', '0'));
        if ($user) {
            \Illuminate\Support\Facades\Auth::setUser($user);
            if ($user->language) {
                app()->setLocale($user->language);
            }
        }
        $panelId = (string) $request->input('panel');
        $panel = null;
        $tenant = null;
        try {
            $panel = Filament::getPanel($panelId);
            Filament::setCurrentPanel($panel);
            if ($panel->hasTenancy() && $request->filled('tenant')) {
                $tenant = \App\Models\Server::query()->where('uuid_short', (string) $request->input('tenant'))->first();
                if ($tenant) {
                    Filament::setTenant($tenant);
                }
            }
        } catch (Throwable) {
            // Pages outside the panels (login): global hooks only.
            $panel = null;
        }
        [$hooks, $banners] = $this->renderHooks($panel, $panelId);
        $tenantReady = $panel && (!$panel->hasTenancy() || $tenant);

        return new JsonResponse([
            'navigation' => $user && $tenantReady ? $this->navigation([$panel], true) : [],
            'hooks' => (object) $hooks,
            'banners' => $banners,
            'console_actions' => $user && $tenant && $panelId === 'server' ? $this->consoleActions() : [],
            'layout' => $user && $tenantReady ? $this->panelNavigation() : [],
        ]);
    }

    /**
     * The panel's whole menu as Filament builds it for the signed user (core and plugin items,
     * group and item order, badges): pelican-rs lays its own menu out the same way.
     *
     * @return array<int, array<string, mixed>>
     */
    private function panelNavigation(): array
    {
        try {
            $out = [];
            foreach (Filament::getNavigation() as $group) {
                if (!$group instanceof \Filament\Navigation\NavigationGroup) {
                    continue;
                }
                $items = [];
                foreach ($group->getItems() as $item) {
                    $color = $item->getBadgeColor();
                    $items[] = [
                        'path' => '/' . ltrim((string) parse_url((string) $item->getUrl(), PHP_URL_PATH), '/'),
                        'badge' => $item->getBadge(),
                        'badge_color' => is_string($color) ? $color : null,
                    ];
                }
                $out[] = ['label' => $this->label($group->getLabel()), 'items' => $items];
            }

            return $out;
        } catch (Throwable $exception) {
            report($exception);

            return [];
        }
    }

    /**
     * The plugins' render hooks for a page of the current panel (HTML by hook name) and the alert
     * banners their `boot()` sends.
     *
     * @return array{0: array<string, string>, 1: array<int, array<string, mixed>>}
     */
    private function renderHooks(?\Filament\Panel $panel, string $panelId): array
    {
        $banners = [];
        $all = [];
        if ($panel) {
            $banners = $this->bootPlugins($panel);
            $all = $this->pluginPanelRenderHooks($panel);
        }
        // Read after the plugins booted: `boot()` may register hooks too.
        $all = array_merge_recursive($all, $this->pluginRenderHooks());
        $out = [];
        foreach ($all as $name => $scopes) {
            $html = '';
            foreach (['', $panelId] as $scope) {
                foreach ($scopes[$scope] ?? [] as $hook) {
                    try {
                        $html .= (string) app()->call($hook, ['data' => [], 'scopes' => [$panelId]]);
                    } catch (Throwable $exception) {
                        report($exception);
                    }
                }
            }
            if ($html !== '') {
                $out[$name] = $html;
            }
        }

        return [$out, $banners];
    }

    /**
     * The plugins' console header actions the signed user can use on the current server.
     *
     * @return array<int, array<string, mixed>>
     */
    private function consoleActions(): array
    {
        $out = [];
        foreach (\PelicanRs\Bridge\Filament\ConsoleActionHost::pluginHeaderActions() as $action) {
            try {
                if ($action->isHidden()) {
                    continue;
                }
                $out[] = [
                    'name' => $action->getName(),
                    'label' => $action->getTooltip() ?: $action->getLabel(),
                    'icon' => $this->iconName($action->getIcon()),
                    'color' => is_string($action->getColor()) ? $action->getColor() : null,
                ];
            } catch (Throwable) {
                // An action that needs the Livewire page to decide: offered, the page decides.
                $out[] = ['name' => $action->getName(), 'label' => $action->getName(), 'icon' => null, 'color' => null];
            }
        }

        return $out;
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
    private function oauthProviders(): array
    {
        $out = [];
        foreach (app(\App\Extensions\OAuth\OAuthService::class)->getAll() as $schema) {
            if ($plugin = Origin::pluginOfClass($schema::class)) {
                $out[] = [
                    'plugin' => $plugin,
                    'id' => $schema->getId(),
                    'name' => $schema->getName(),
                    'icon' => $this->iconName($schema->getIcon()),
                    'color' => $schema->getHexColor(),
                    'enabled' => $schema->isEnabled(),
                ];
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
