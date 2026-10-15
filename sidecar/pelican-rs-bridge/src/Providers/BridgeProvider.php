<?php

namespace PelicanRs\Bridge\Providers;

use Illuminate\Console\Events\CommandStarting;
use Illuminate\Console\Scheduling\Schedule;
use Illuminate\Support\Facades\Event;
use Illuminate\Support\Facades\Route;
use Illuminate\Support\ServiceProvider;
use PelicanRs\Bridge\Http\Controllers\BridgeController;
use PelicanRs\Bridge\Http\Middleware\VerifyPelicanRs;
use PelicanRs\Bridge\Origin;

class BridgeProvider extends ServiceProvider
{
    public function boot(): void
    {
        // Stateless endpoints for pelican-rs only (no session, no CSRF).
        Route::middleware([VerifyPelicanRs::class])
            ->prefix('_bridge')
            ->group(function () {
                Route::get('manifest', [BridgeController::class, 'manifest']);
                Route::post('event', [BridgeController::class, 'event']);
                Route::post('task', [BridgeController::class, 'task']);
                Route::post('artisan', [BridgeController::class, 'artisan']);
                Route::post('avatar', [BridgeController::class, 'avatar']);
                Route::get('lang', [BridgeController::class, 'lang']);
                Route::post('navigation', [BridgeController::class, 'userNavigation']);
                Route::post('hooks', [BridgeController::class, 'renderHooks']);
                Route::post('console-actions', [BridgeController::class, 'consoleActions']);
            });

        // Real sign-ins inside the sidecar are handed to pelican-rs (LoginHandoff); the bridge's
        // own per-request `Auth::setUser()` fires no `Login` event.
        Event::listen(\Illuminate\Auth\Events\Login::class, function ($event) {
            app()->instance('pelican-rs.login', $event->user->getAuthIdentifier());
        });
        $this->app->make(\Illuminate\Contracts\Http\Kernel::class)
            ->pushMiddleware(\PelicanRs\Bridge\Http\Middleware\LoginHandoff::class);
        // The OAuth callback links a provider to the signed-in pelican-rs user.
        $this->app->booted(function () {
            Route::getRoutes()->getByName('auth.oauth.callback')
                ?->middleware(\PelicanRs\Bridge\Http\Middleware\AuthenticateFromPelicanRs::class);
        });

        // pelican-rs runs the core schedule; the sidecar keeps only what plugins add. The
        // schedule is complete once every provider's resolving callback ran, i.e. when
        // `schedule:*` starts.
        Event::listen(CommandStarting::class, function (CommandStarting $event) {
            if (str_starts_with((string) $event->command, 'schedule:')) {
                Origin::keepPluginSchedule($this->app->make(Schedule::class));
            }
        });
    }
}
