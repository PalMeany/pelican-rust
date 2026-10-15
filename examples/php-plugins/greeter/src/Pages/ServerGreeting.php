<?php

namespace Example\Greeter\Pages;

use App\Models\Server;
use Filament\Facades\Filament;
use Filament\Pages\Page;
use Filament\Support\Icons\Heroicon;

class ServerGreeting extends Page
{
    protected static string|\BackedEnum|null $navigationIcon = Heroicon::OutlinedHandRaised;

    protected static ?string $slug = 'greeter';

    protected string $view = 'greeter::server-greeting';

    public static function getNavigationLabel(): string
    {
        return trans('greeter::greeter.nav');
    }

    public function getViewData(): array
    {
        /** @var Server $server */
        $server = Filament::getTenant();

        return ['server' => $server->name, 'user' => auth()->user()?->username];
    }
}
