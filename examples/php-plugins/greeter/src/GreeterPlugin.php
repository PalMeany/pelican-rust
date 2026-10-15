<?php

namespace Example\Greeter;

use Example\Greeter\Pages\Greeting;
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
        if ($panel->getId() === 'admin') {
            $panel->pages([Greeting::class]);
        }
    }

    public function boot(Panel $panel): void {}
}
