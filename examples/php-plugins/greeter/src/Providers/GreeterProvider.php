<?php

namespace Example\Greeter\Providers;

use App\Enums\HeaderActionPosition;
use App\Extensions\Tasks\TaskService;
use App\Filament\Server\Pages\Console;
use Filament\Actions\Action;
use Filament\Notifications\Notification;
use App\Models\Server;
use Example\Greeter\Tasks\GreetTask;
use Illuminate\Support\Facades\Event;
use Illuminate\Support\Facades\Route;
use Illuminate\Support\ServiceProvider;

class GreeterProvider extends ServiceProvider
{
    public function register(): void
    {
        $this->callAfterResolving(TaskService::class, fn (TaskService $tasks) => $tasks->register(new GreetTask()));

        // A console header action (shown on pelican-rs's console through the bridge).
        Console::registerCustomHeaderActions(HeaderActionPosition::Before, Action::make('greeter_wave')
            ->icon('tabler-hand-stop')
            ->tooltip('Wave')
            ->requiresConfirmation()
            ->action(fn () => Notification::make()->title('The greeter waves back')->success()->send()));
    }

    public function boot(): void
    {
        Route::get('greeter/ping', fn () => response()->json([
            'pong' => true,
            'last_server_update' => cache()->get('greeter.last_server_update'),
            'last_task' => cache()->get('greeter.last_task'),
        ]));

        Event::listen('eloquent.updated: ' . Server::class, function (Server $server) {
            cache()->forever('greeter.last_server_update', $server->name);
        });
    }
}
