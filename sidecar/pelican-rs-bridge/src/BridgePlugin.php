<?php

namespace PelicanRs\Bridge;

use Filament\Contracts\Plugin;
use Filament\Panel;
use PelicanRs\Bridge\Http\Middleware\AuthenticateFromPelicanRs;

class BridgePlugin implements Plugin
{
    public function getId(): string
    {
        return 'pelican-rs-bridge';
    }

    public function register(Panel $panel): void
    {
        // Runs after StartSession and before the panel's Authenticate; persistent so that
        // Livewire update requests of the panel's components pass through it too.
        $panel->middleware([AuthenticateFromPelicanRs::class], isPersistent: true);
        if ($panel->getId() === 'server') {
            $panel->pages([Filament\ConsoleActionHost::class]);
        }
    }

    public function boot(Panel $panel): void {}
}
