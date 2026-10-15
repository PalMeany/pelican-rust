<?php

namespace PelicanRs\Bridge\Http\Middleware;

use App\Models\User;
use Closure;
use Illuminate\Http\Request;
use Illuminate\Support\Facades\Auth;
use PelicanRs\Bridge\Signature;
use Symfony\Component\HttpFoundation\Response;

/**
 * pelican-rs owns sign-in: every proxied request names the signed-in user (if any) in a
 * signed header.
 */
class AuthenticateFromPelicanRs
{
    public function handle(Request $request, Closure $next): Response
    {
        abort_unless(Signature::validProxy($request), 403);

        // pelican-rs shows the plugins' pages inside its own layout: links load whole pages
        // through it instead of Livewire's SPA navigation swapping the sidecar's layout in.
        \Filament\Support\Facades\FilamentView::spa(false);

        // Per request, without `Login` events (the original logs those as activity;
        // pelican-rs already did when the user signed in).
        $id = (int) $request->header('X-Pelican-Rs-User', '0');
        if ($id === 0) {
            // A guest in pelican-rs is a guest here: drop a sign-in left in the sidecar's own
            // session (registration, OAuth) without the original's logout side effects.
            $guard = Auth::guard();
            if ($guard instanceof \Illuminate\Auth\SessionGuard && $request->hasSession()) {
                $request->session()->forget($guard->getName());
                $guard->forgetUser();
            }
        } else {
            $user = User::query()->find($id);
            abort_if(is_null($user), 403);
            Auth::setUser($user);
        }

        return $next($request);
    }
}
