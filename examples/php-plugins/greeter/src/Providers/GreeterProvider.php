<?php

namespace Example\Greeter\Providers;

use App\Extensions\Tasks\TaskService;
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
