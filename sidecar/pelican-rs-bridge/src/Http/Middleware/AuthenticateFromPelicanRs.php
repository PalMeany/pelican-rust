<?php

namespace PelicanRs\Bridge\Http\Middleware;

use App\Models\User;
use Closure;
use Illuminate\Http\Request;
use Illuminate\Support\Facades\Auth;
use PelicanRs\Bridge\Signature;
use Symfony\Component\HttpFoundation\Response;

/**
 * pelican-rs owns sign-in: every proxied request names the signed-in user in a signed
 * header, and the sidecar's own session follows it.
 */
class AuthenticateFromPelicanRs
{
    public function handle(Request $request, Closure $next): Response
    {
        abort_unless(Signature::valid($request, false), 403);

        $id = (int) $request->header('X-Pelican-Rs-User', '0');
        if ($id === 0) {
            if (Auth::check()) {
                Auth::logout();
            }
        } elseif (Auth::id() !== $id) {
            $user = User::query()->find($id);
            abort_if(is_null($user), 403);
            Auth::login($user);
        }

        return $next($request);
    }
}
