import axios from "axios";
import { DataObject, OAuthResponse } from "../../lib/types";
import { generateBasicHeaders } from "../../lib/helpers";

export const init = async ({ body }: DataObject): Promise<OAuthResponse> => {
  try {
    const requestBody = {
      grant_type: "authorization_code",
      code: body.metadata?.code,
      redirect_uri: body.metadata?.redirectUri,
    };

    const response = await axios.post(
      "https://oauth.platform.intuit.com/oauth2/v1/tokens/bearer",
      requestBody,
      {
        headers: {
          ...generateBasicHeaders(body.clientId, body.clientSecret),
          Accept: "application/json",
        },
      }
    );

    const {
      data: {
        access_token: accessToken,
        refresh_token: refreshToken,
        expires_in: expiresIn,
        token_type: tokenType,
      },
    } = response;

    return {
      accessToken,
      refreshToken,
      expiresIn,
      tokenType,
      meta: {
        realmId: body.metadata?.additionalData?.realmId,
      },
    };
  } catch (error) {
    throw new Error(`Error fetching access token for Quickbooks: ${error}`);
  }
};
