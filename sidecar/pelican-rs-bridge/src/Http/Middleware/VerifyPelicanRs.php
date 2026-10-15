<?php

namespace PelicanRs\Bridge\Http\Middleware;

use Closure;
use Illuminate\Http\Request;
use PelicanRs\Bridge\Signature;
use Symfony\Component\HttpFoundation\Response;

/** Bridge endpoints: signed by pelican-rs, body included. */
class VerifyPelicanRs
{
    public function handle(Request $request, Closure $next): Response
    {
        abort_unless(Signature::valid($request), 403);

        return $next($request);
    }
}
