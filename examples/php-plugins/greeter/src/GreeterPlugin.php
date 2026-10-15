<?php

namespace Example\Greeter;

use Example\Greeter\Pages\Greeting;
use Example\Greeter\Pages\ServerGreeting;
use Filament\Contracts\Plugin;
use Filament\Panel;

class GreeterPlugin implements Plugin
{
    public function getId(): string
    {
        return 'greeter';
    }

    public function register(Panel $panel): void
    {
        match ($panel->getId()) {
            'admin' => $panel->pages([Greeting::class]),
            'server' => $panel->pages([ServerGreeting::class]),
            default => null,
        };
    }

    public function boot(Panel $panel): void {}
}
