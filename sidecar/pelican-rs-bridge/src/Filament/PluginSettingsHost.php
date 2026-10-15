<?php

namespace PelicanRs\Bridge\Filament;

use App\Enums\PluginStatus;
use App\Models\Plugin;
use Filament\Actions\Action;
use Filament\Pages\Page;

/**
 * The "Settings" action of the original's plugin list (`HasPluginSettings`) for PHP plugins,
 * opened from pelican-rs's Admin › Plugins as `?plugin=<id>`: the plugin's own form, filled
 * with `getSettingsFormData()` and saved with `saveSettings()`.
 */
class PluginSettingsHost extends Page
{
    protected static ?string $slug = 'rs-plugin-settings';

    protected static bool $shouldRegisterNavigation = false;

    protected string $view = 'pelican-rs-bridge::plugin-settings';

    public string $plugin = '';

    public function mount(): void
    {
        $this->plugin = (string) request()->query('plugin');
        $model = self::configurable($this->plugin);
        abort_if(is_null($model), 404);
        abort_unless(user()?->can('update', $model), 403);
        $this->defaultAction = 'settings';
    }

    /** An enabled plugin with settings, as the original shows the action. */
    public static function configurable(string $id): ?Plugin
    {
        $model = Plugin::find($id);

        return $model && $model->status === PluginStatus::Enabled && $model->hasSettings() ? $model : null;
    }

    public function getTitle(): string
    {
        return self::configurable($this->plugin)?->name ?? trans('admin/plugin.settings');
    }

    public function settingsAction(): Action
    {
        return Action::make('settings')
            ->label(trans('admin/plugin.settings'))
            ->authorize(fn () => user()?->can('update', self::configurable($this->plugin)))
            ->schema(fn () => self::configurable($this->plugin)?->getSettingsForm() ?? [])
            ->fillForm(fn () => self::configurable($this->plugin)?->getSettingsFormData() ?? [])
            ->action(fn (array $data) => self::configurable($this->plugin)?->saveSettings($data))
            ->slideOver();
    }

    protected function getHeaderActions(): array
    {
        return [$this->settingsAction()];
    }
}
