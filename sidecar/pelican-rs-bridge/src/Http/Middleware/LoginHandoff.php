<?php

namespace PelicanRs\Bridge\Http\Middleware;

use Closure;
use Illuminate\Http\Request;
use PelicanRs\Bridge\Signature;
use Symfony\Component\HttpFoundation\Response;

/**
 * A real sign-in inside the sidecar (OAuth callback of a plugin provider, a plugin's
 * registration page) is handed to pelican-rs, which owns the session: the response
 * carries `X-Pelican-Rs-Login: <user id>.<timestamp>.<hmac>`.
 */
class LoginHandoff
{
    public function handle(Request $request, Closure $next): Response
    {
        $response = $next($request);
        if (app()->bound('pelican-rs.login')) {
            $id = (int) app('pelican-rs.login');
            $ts = time();
            $sig = hash_hmac('sha256', "login\n$id\n$ts", Signature::secret());
            $response->headers->set('X-Pelican-Rs-Login', "$id.$ts.$sig");
        }

        return $response;
    }
}
