<?php

namespace Example\Greeter\Pages;

use App\Models\Server;
use Filament\Pages\Page;
use Filament\Support\Icons\Heroicon;

class Greeting extends Page
{
    protected static string|\BackedEnum|null $navigationIcon = Heroicon::OutlinedHandRaised;

    protected static ?string $slug = 'greeter';

    protected string $view = 'greeter::greeting';

    public int $clicks = 0;

    public static function getNavigationLabel(): string
    {
        return trans('greeter::greeter.nav');
    }

    public function getTitle(): string
    {
        return trans('greeter::greeter.title');
    }

    public function click(): void
    {
        $this->clicks++;
    }

    public function getViewData(): array
    {
        return [
            'user' => auth()->user()?->username,
            'servers' => Server::query()->count(),
            'last' => cache()->get('greeter.last_server_update'),
        ];
    }
}
