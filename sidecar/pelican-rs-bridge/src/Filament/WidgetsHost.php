<?php

namespace PelicanRs\Bridge\Filament;

use Filament\Facades\Filament;
use Filament\Pages\Dashboard;
use Filament\Widgets\WidgetConfiguration;
use PelicanRs\Bridge\Origin;

/**
 * The widgets PHP plugins add to the admin panel (`$panel->widgets()`), which the original shows
 * on its dashboard: pelican-rs renders its own dashboard and shows this page's content below it.
 */
class WidgetsHost extends Dashboard
{
    protected static string $routePath = '/rs-widgets';

    protected static ?string $slug = 'rs-widgets';

    protected static bool $shouldRegisterNavigation = false;

    /** @return array<class-string<\Filament\Widgets\Widget> | WidgetConfiguration> */
    public static function pluginWidgets(?\Filament\Panel $panel = null): array
    {
        $widgets = ($panel ?? Filament::getCurrentOrDefaultPanel())->getWidgets();

        return array_values(array_filter($widgets, function ($widget): bool {
            $class = $widget instanceof WidgetConfiguration ? $widget->widget : $widget;

            return !is_null(Origin::pluginOfClass($class));
        }));
    }

    public function getWidgets(): array
    {
        return self::pluginWidgets();
    }

    /** The original's admin dashboard: one column. */
    public function getColumns(): int|array
    {
        return 1;
    }

    public function getTitle(): string
    {
        return '';
    }

    public function getHeading(): string
    {
        return '';
    }
}
