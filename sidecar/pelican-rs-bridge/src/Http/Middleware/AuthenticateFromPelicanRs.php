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

        // Per request, without `Login` events (the original logs those as activity;
        // pelican-rs already did when the user signed in).
        $id = (int) $request->header('X-Pelican-Rs-User', '0');
        if ($id !== 0) {
            $user = User::query()->find($id);
            abort_if(is_null($user), 403);
            Auth::setUser($user);
        }

        return $next($request);
    }
}
