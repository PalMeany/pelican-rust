<?php

namespace PelicanRs\Bridge\Filament;

use App\Extensions\Features\FeatureService;
use App\Filament\Server\Pages\Console;
use App\Models\Server;
use Filament\Actions\Action;
use Filament\Facades\Filament;
use Filament\Pages\Page;
use PelicanRs\Bridge\Origin;
use ReflectionProperty;
use Throwable;

/**
 * Runs, for pelican-rs's console, the Filament actions PHP plugins add to the original's
 * console: header actions (`Console::registerCustomHeaderActions()`) and server features
 * (`FeatureSchemaInterface::getAction()`). Opened as `?action=<name>` (Filament's default
 * action) or `?feature=<id>`.
 */
class ConsoleActionHost extends Page
{
    protected static ?string $slug = 'rs-console-action';

    protected static bool $shouldRegisterNavigation = false;

    protected string $view = 'pelican-rs-bridge::console-action';

    public function mount(): void
    {
        // A run pelican-rs's console signed for this user, server and action (`rs_run`): the
        // action starts at once, also without a modal. Unsigned `?action=` links only open it.
        $action = request()->query('action');
        $run = (string) request()->query('rs_run', '');
        if (is_string($action) && $run !== '' && self::validRun($run, $action)) {
            $this->mountAction($action);

            return;
        }

        $feature = request()->query('feature');
        if (is_string($feature) && ($action = self::featureAction($feature))) {
            $this->defaultAction = $action->getName();
        }
    }

    /** `<ts>.<hex hmac("console-action\n<user>\n<server>\n<action>\n<ts>")>`, for twelve hours. */
    public static function validRun(string $run, string $action): bool
    {
        [$ts, $signature] = array_pad(explode('.', $run, 2), 2, '');
        if (!ctype_digit($ts) || time() - (int) $ts > 43200 || (int) $ts > time() + 60 || $signature === '') {
            return false;
        }
        $message = implode("\n", ['console-action', (string) auth()->id(), (string) Filament::getTenant()?->uuid_short, $action, $ts]);

        return hash_equals(hash_hmac('sha256', $message, \PelicanRs\Bridge\Signature::secret()), $signature);
    }

    public function getTitle(): string
    {
        return Filament::getTenant()?->name ?? 'Console';
    }

    /** @return Action[] plugin header actions of the original's console */
    public static function pluginHeaderActions(): array
    {
        $registry = (new ReflectionProperty(Console::class, 'customHeaderActions'))->getValue();
        $out = [];
        foreach ($registry as $actions) {
            foreach ($actions as $action) {
                // Only plugins use this registry; the core's actions are getDefaultHeaderActions().
                if ($action instanceof Action) {
                    $out[] = $action;
                }
            }
        }

        return $out;
    }

    public static function featureAction(string $id): ?Action
    {
        $schema = app(FeatureService::class)->get($id);
        if (is_null($schema) || is_null(Origin::pluginOfClass($schema::class))) {
            return null;
        }
        /** @var Server|null $server */
        $server = Filament::getTenant();
        $user = auth()->user();
        try {
            if ($server && $user && !$schema->authorize($user, $server)) {
                return null;
            }

            return $schema->getAction();
        } catch (Throwable) {
            return null;
        }
    }

    protected function getHeaderActions(): array
    {
        $actions = self::pluginHeaderActions();
        $feature = request()->query('feature');
        if (is_string($feature) && ($action = self::featureAction($feature))) {
            $actions[] = $action;
        }

        return $actions;
    }
}
