<?php

namespace PelicanRs\Bridge\Filament;

use Filament\Facades\Filament;
use Filament\Pages\Page;
use Illuminate\Database\Eloquent\Model;
use PelicanRs\Bridge\Origin;
use ReflectionProperty;
use Throwable;

/**
 * The relation managers PHP plugins add to the admin panel's resources
 * (`XResource::registerCustomRelations()`, e.g. a server's subdomains), for one record:
 * `?resource=<slug>&record=<key>`. pelican-rs shows this page's content on its own edit page.
 */
class RelationsHost extends Page
{
    protected static ?string $slug = 'rs-relations';

    protected static bool $shouldRegisterNavigation = false;

    protected string $view = 'pelican-rs-bridge::relations';

    public string $resource = '';

    public string $record = '';

    public function mount(): void
    {
        $this->resource = (string) request()->query('resource');
        $this->record = (string) request()->query('record');
        $class = self::resourceClass($this->resource);
        abort_if(is_null($class), 404);
        abort_unless($class::canEdit($this->ownerRecord()), 403);
    }

    /** @return class-string<\Filament\Resources\Resource>|null */
    public static function resourceClass(string $slug): ?string
    {
        foreach (Filament::getPanel('admin')->getResources() as $class) {
            if ($class::getSlug() === $slug) {
                return $class;
            }
        }

        return null;
    }

    /**
     * Relation managers plugins registered on the admin panel's resources: slug => classes.
     *
     * @return array<string, class-string[]>
     */
    public static function pluginRelations(): array
    {
        $out = [];
        foreach (Filament::getPanel('admin')->getResources() as $class) {
            try {
                $custom = (new ReflectionProperty($class, 'customRelations'))->getValue();
            } catch (Throwable) {
                continue;
            }
            $managers = array_values(array_filter((array) $custom, fn ($m) => is_string($m) && !is_null(Origin::pluginOfClass($m))));
            if ($managers !== []) {
                $out[$class::getSlug()] = $managers;
            }
        }

        return $out;
    }

    public function ownerRecord(): Model
    {
        $class = self::resourceClass($this->resource);

        return $class::getModel()::query()->findOrFail($this->record);
    }

    /** @return class-string[] */
    public function managers(): array
    {
        return self::pluginRelations()[$this->resource] ?? [];
    }

    /** The resource's edit page, which relation managers take as their page. */
    public function pageClass(): string
    {
        $class = self::resourceClass($this->resource);
        $pages = $class::getPages();

        return isset($pages['edit']) ? $pages['edit']->getPage() : static::class;
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
